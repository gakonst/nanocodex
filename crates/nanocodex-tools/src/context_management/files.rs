//! Native workspace storage. Files are replaced atomically before context is discarded.
use super::{BackendFuture, HistoryNotesHost, StorageOperation};
use serde_json::{Value, json};
use std::{
    path::{Component, PathBuf},
    sync::Arc,
};

pub fn host(workspace: impl Into<PathBuf>) -> Arc<dyn HistoryNotesHost> {
    Arc::new(FileHost {
        workspace: workspace.into(),
    })
}

struct FileHost {
    workspace: PathBuf,
}

impl HistoryNotesHost for FileHost {
    fn available(&self, _thread_id: String) -> BackendFuture<Result<bool, String>> {
        Box::pin(async { Ok(true) })
    }
    fn access(
        &self,
        _thread_id: String,
        operation: StorageOperation,
    ) -> BackendFuture<Result<Value, String>> {
        let workspace = self.workspace.clone();
        Box::pin(async move {
            // Keep each filesystem operation on one blocking worker instead
            // of dispatching every path component and directory entry.
            tokio::task::spawn_blocking(move || access(workspace, operation))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())
        })
    }
}

fn access(workspace: PathBuf, operation: StorageOperation) -> std::io::Result<Value> {
    use std::fs;
    let relative = match &operation {
        StorageOperation::Read { path }
        | StorageOperation::Write { path, .. }
        | StorageOperation::List { path } => path,
    };
    let mut path = workspace;
    for part in std::path::Path::new(relative).components() {
        let Component::Normal(name) = part else {
            return Err(std::io::Error::other("Context path escaped its workspace"));
        };
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(std::io::Error::other(
                    "Context storage refuses symbolic links",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match operation {
        StorageOperation::Read { .. } => match fs::read_to_string(path) {
            Ok(text) => Ok(json!(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
            Err(error) => Err(error),
        },
        StorageOperation::Write { contents, .. } => {
            use std::io::Write;
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let parent = path.parent().expect("context file has a parent");
            fs::create_dir_all(parent)?;
            let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary)?;
            file.write_all(contents.as_bytes())?;
            drop(file);
            fs::rename(&temporary, path)?;
            Ok(Value::Null)
        }
        StorageOperation::List { path: relative } => {
            let directory = match fs::read_dir(path) {
                Ok(directory) => directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!([])),
                Err(error) => return Err(error),
            };
            let mut entries = Vec::new();
            for entry in directory {
                let entry = entry?;
                if entry.file_type()?.is_file()
                    && entry.path().extension().is_some_and(|ext| ext == "json")
                {
                    entries.push(format!(
                        "{}/{}",
                        relative.trim_end_matches('/'),
                        entry.file_name().to_string_lossy()
                    ));
                }
            }
            entries.sort();
            Ok(json!(entries))
        }
    }
}
