use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use crate::AgentError;

const MAX_PROJECT_INSTRUCTIONS_BYTES: usize = 32 * 1024;
const CANDIDATE_FILENAMES: [&str; 2] = ["AGENTS.override.md", "AGENTS.md"];

pub(super) fn load_project_instructions(workspace: &Path) -> Result<Option<String>, AgentError> {
    let root = find_project_root(workspace)?;
    let mut directories = workspace
        .ancestors()
        .take_while(|directory| *directory != root)
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    directories.push(root);
    directories.reverse();

    let mut remaining = MAX_PROJECT_INSTRUCTIONS_BYTES;
    let mut documents = Vec::new();
    for directory in directories {
        if remaining == 0 {
            break;
        }
        let Some(path) = instruction_file(&directory)? else {
            continue;
        };
        let data = read_bounded(&path, remaining).map_err(|source| {
            AgentError::ReadProjectInstructions {
                path: path.clone(),
                source,
            }
        })?;
        remaining -= data.len();
        let text = String::from_utf8_lossy(&data).into_owned();
        if !text.trim().is_empty() {
            documents.push(text);
        }
    }

    Ok((!documents.is_empty()).then(|| documents.join("\n\n")))
}

fn find_project_root(workspace: &Path) -> Result<PathBuf, AgentError> {
    for directory in workspace.ancestors() {
        let marker = directory.join(".git");
        match fs::metadata(&marker) {
            Ok(_) => return Ok(directory.to_path_buf()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(AgentError::ReadProjectInstructions {
                    path: marker,
                    source,
                });
            }
        }
    }
    Ok(workspace.to_path_buf())
}

fn instruction_file(directory: &Path) -> Result<Option<PathBuf>, AgentError> {
    for filename in CANDIDATE_FILENAMES {
        let path = directory.join(filename);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(path)),
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(AgentError::ReadProjectInstructions { path, source });
            }
        }
    }
    Ok(None)
}

fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut data = Vec::with_capacity(limit.min(8 * 1024));
    file.take(limit as u64).read_to_end(&mut data)?;
    Ok(data)
}
