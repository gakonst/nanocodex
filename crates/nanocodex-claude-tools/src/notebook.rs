//! Explicitly rooted, bounded Jupyter notebook cell edits.
//!
//! The caller must authorize and isolate `root` before constructing this adapter.
//! Lexical and symlink checks are defense in depth: a hostile process swapping
//! directory entries concurrently still requires OS-level workspace isolation.

use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::portable_files::MAX_FILE as MAX_NOTEBOOK;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// NotebookEdit adapter scoped to an explicitly authorized directory.
#[derive(Clone, Debug)]
pub struct ClaudeNotebook {
    root: PathBuf,
    root_alias: PathBuf,
}

impl ClaudeNotebook {
    /// Construct from an existing, authorized workspace directory.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let given = root.as_ref();
        let root_alias = if given.is_absolute() {
            given.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("notebook current directory: {e}"))?
                .join(given)
        };
        let root = fs::canonicalize(given).map_err(|e| format!("notebook workspace root: {e}"))?;
        if !root.is_dir() {
            return Err("notebook workspace root is not a directory".into());
        }
        Ok(Self { root, root_alias })
    }

    /// Standalone NotebookEdit metadata; no Codex tool contract is used.
    #[must_use]
    pub fn definitions() -> Vec<Value> {
        crate::portable_notebook::definitions()
    }

    /// Alias for [`Self::definitions`].
    #[must_use]
    pub fn tool_schemas() -> Vec<Value> {
        Self::definitions()
    }

    /// Execute NotebookEdit; returns a short outcome or a descriptive validation error.
    pub async fn execute(&self, name: &str, input: Value) -> Result<String, String> {
        if name != "NotebookEdit" {
            return Err(format!("unknown notebook tool: {name}"));
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.edit(&input))
            .await
            .map_err(|e| format!("notebook task: {e}"))?
    }

    fn field<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
        input
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing or invalid {key}"))
    }

    fn notebook_path(&self, raw: &str) -> Result<PathBuf, String> {
        if raw.is_empty() || raw.len() > 4096 {
            return Err("notebook_path must be 1..4096 bytes".into());
        }
        let path = Path::new(raw);
        let rel = if path.is_absolute() {
            path.strip_prefix(&self.root)
                .or_else(|_| path.strip_prefix(&self.root_alias))
                .map_err(|_| "notebook_path is outside workspace".to_string())?
        } else {
            path
        };
        if rel.as_os_str().is_empty()
            || rel.components().any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("notebook_path contains traversal or special components".into());
        }
        if rel.extension().is_none_or(|ext| ext != "ipynb") {
            return Err("notebook_path must end in .ipynb".into());
        }
        let mut checked = self.root.clone();
        for component in rel.components() {
            checked.push(component.as_os_str());
            let meta = fs::symlink_metadata(&checked).map_err(|e| format!("notebook path: {e}"))?;
            if meta.file_type().is_symlink() {
                return Err("notebook path uses a symlink".into());
            }
        }
        if !checked.is_file() {
            return Err("notebook path is not a regular file".into());
        }
        Ok(checked)
    }

    fn edit(&self, input: &Value) -> Result<String, String> {
        let path = self.notebook_path(Self::field(input, "notebook_path")?)?;
        let before = read_bounded(&path)?;
        let (output, mut result) = crate::portable_notebook::edit(input, &before)?;
        result["notebook_path"] = json!(path.display().to_string());
        if read_bounded(&path)? != before
            || fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("notebook changed during edit".into());
        }
        // Preserve the JSON result contract while supplying the same bounded
        // path-scoped guidance as the other native workspace operations.
        if let Ok(loader) = crate::ClaudeProjectContext::new(&self.root) {
            let context = loader.load_for_path(path.strip_prefix(&self.root).unwrap_or(&path));
            if !context.excerpts.is_empty() || !context.diagnostics.is_empty() {
                result["project_context"] = json!(context);
            }
        }
        let result = result.to_string();
        if result.len() > 1024 * 1024 {
            return Err("NotebookEdit result with project context exceeds 1 MiB".into());
        }
        atomic_write(&path, &output)?;
        Ok(result)
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| format!("read notebook: {e}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_NOTEBOOK + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read notebook: {e}"))?;
    if bytes.len() > MAX_NOTEBOOK {
        return Err("notebook exceeds 1 MiB limit".into());
    }
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("notebook has no parent")?;
    let permissions = fs::metadata(path)
        .map_err(|e| format!("notebook metadata: {e}"))?
        .permissions();
    for _ in 0..32 {
        let nonce = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".notebookedit-{}-{nonce}.tmp", std::process::id()));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("create notebook temporary file: {e}")),
        };
        let result = (|| {
            file.set_permissions(permissions)
                .map_err(|e| format!("temporary permissions: {e}"))?;
            file.write_all(bytes)
                .map_err(|e| format!("write notebook: {e}"))?;
            file.sync_all().map_err(|e| format!("sync notebook: {e}"))?;
            // Reject changed target symlinks again, immediately before rename.
            if fs::symlink_metadata(path)
                .map_err(|e| format!("notebook path: {e}"))?
                .file_type()
                .is_symlink()
            {
                return Err("notebook path uses a symlink".into());
            }
            fs::rename(&temp, path).map_err(|e| format!("replace notebook: {e}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        return result;
    }
    Err("could not allocate notebook temporary file".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, ClaudeNotebook, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.ipynb");
        fs::write(&path, serde_json::to_vec(&json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {"kernelspec": {"name": "python3"}},
            "cells": [
                {"id":"a", "cell_type":"code", "metadata":{"tag":"keep"}, "source":["x=1\n"], "execution_count":3,"outputs":[{"output_type":"stream","text":"1"}]},
                {"id":"b", "cell_type":"markdown", "metadata":{"tag":"also keep"}, "source":["hi"]}
            ]
        })).unwrap()).unwrap();
        let adapter = ClaudeNotebook::new(dir.path()).unwrap();
        (dir, adapter, path)
    }

    #[tokio::test]
    async fn unsupported_mutation_options_do_not_change_notebook() {
        let (_dir, api, path) = setup();
        let before = fs::read(&path).unwrap();
        assert!(
            api.execute(
                "NotebookEdit",
                json!({"notebook_path":path,"new_source":"bad","cell_id":"a","editMode":"insert"})
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn replace_insert_delete_preserve_metadata() {
        let (_dir, api, path) = setup();
        let result = api
            .execute(
                "NotebookEdit",
                json!({"notebook_path":path,"new_source":"print(2)\n","cell_id":"a"}),
            )
            .await
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["old_source"], "x=1\n");
        assert_eq!(result["new_source"], "print(2)\n");
        assert_eq!(result["cell_id"], "a");
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["metadata"]["kernelspec"]["name"], "python3");
        assert_eq!(v["cells"][0]["metadata"]["tag"], "keep");
        assert_eq!(v["cells"][0]["outputs"][0]["text"], "1");
        assert_eq!(v["cells"][0]["source"], json!(["print(2)\n"]));
        api.execute("NotebookEdit", json!({"notebook_path":path,"new_source":"title", "edit_mode":"insert", "cell_type":"markdown"})).await.unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["cells"][0]["source"], json!(["title"]));
        api.execute(
            "NotebookEdit",
            json!({"notebook_path":path,"new_source":"", "edit_mode":"delete", "cell_id":"b"}),
        )
        .await
        .unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["cells"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn validation_and_no_clobber() {
        let (dir, api, path) = setup();
        let before = fs::read(&path).unwrap();
        assert!(
            api.edit(&json!({"notebook_path":"../note.ipynb","new_source":"x"}))
                .is_err()
        );
        assert!(
            api.edit(&json!({"notebook_path":path,"new_source":"x","cell_id":"missing"}))
                .is_err()
        );
        assert!(
            api.edit(&json!({"notebook_path":path,"new_source":"x","edit_mode":"invalid"}))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let big = dir.path().join("large.ipynb");
        fs::write(&big, vec![b'x'; MAX_NOTEBOOK + 1]).unwrap();
        assert!(
            api.edit(&json!({"notebook_path":big,"new_source":"x"}))
                .unwrap_err()
                .contains("1 MiB")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_rejected() {
        use std::os::unix::fs::symlink;
        let (dir, api, path) = setup();
        symlink(&path, dir.path().join("alias.ipynb")).unwrap();
        assert!(
            api.edit(&json!({"notebook_path":"alias.ipynb","new_source":"x"}))
                .unwrap_err()
                .contains("symlink")
        );
        fs::create_dir(dir.path().join("real")).unwrap();
        symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        fs::copy(&path, dir.path().join("real").join("nested.ipynb")).unwrap();
        assert!(
            api.edit(&json!({"notebook_path":"link/nested.ipynb","new_source":"x"}))
                .unwrap_err()
                .contains("symlink")
        );
    }
}
