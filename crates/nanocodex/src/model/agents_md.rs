use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use tracing::warn;

use crate::NanocodexError;

const MAX_PROJECT_INSTRUCTIONS_BYTES: usize = 32 * 1024;
const CANDIDATE_FILENAMES: [&str; 2] = ["AGENTS.override.md", "AGENTS.md"];
const PROJECT_DOC_SEPARATOR: &str = "\n\n--- project-doc ---\n\n";

pub(crate) fn load_global_instructions(codex_home: Option<&Path>) -> Option<Arc<str>> {
    let codex_home = codex_home?;
    for filename in CANDIDATE_FILENAMES {
        let path = codex_home.join(filename);
        match fs::metadata(&path) {
            Ok(metadata) if !metadata.is_file() => continue,
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                warn!(
                    path = %path.display(),
                    error = %source,
                    "failed to read global AGENTS.md instructions"
                );
                continue;
            }
        }
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                warn!(
                    path = %path.display(),
                    error = %source,
                    "failed to read global AGENTS.md instructions"
                );
                continue;
            }
        };
        let text = String::from_utf8_lossy(&data);
        let text = text.trim();
        if !text.is_empty() {
            return Some(Arc::from(text));
        }
    }
    None
}

pub(super) fn load_instructions(
    workspace: &Path,
    global_instructions: Option<&str>,
) -> Result<Option<String>, NanocodexError> {
    let project_instructions = load_project_instructions(workspace)?;
    Ok(match (global_instructions, project_instructions) {
        (Some(global), Some(project)) => Some(format!("{global}{PROJECT_DOC_SEPARATOR}{project}")),
        (Some(global), None) => Some(global.to_owned()),
        (None, project) => project,
    })
}

fn load_project_instructions(workspace: &Path) -> Result<Option<String>, NanocodexError> {
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
            NanocodexError::ReadProjectInstructions {
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

fn find_project_root(workspace: &Path) -> Result<PathBuf, NanocodexError> {
    for directory in workspace.ancestors() {
        let marker = directory.join(".git");
        match fs::metadata(&marker) {
            Ok(_) => return Ok(directory.to_path_buf()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(NanocodexError::ReadProjectInstructions {
                    path: marker,
                    source,
                });
            }
        }
    }
    Ok(workspace.to_path_buf())
}

fn instruction_file(directory: &Path) -> Result<Option<PathBuf>, NanocodexError> {
    for filename in CANDIDATE_FILENAMES {
        let path = directory.join(filename);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(path)),
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(NanocodexError::ReadProjectInstructions { path, source });
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn missing_global_files_return_no_instructions() {
        let home = tempdir().unwrap();

        assert!(load_global_instructions(Some(home.path())).is_none());
    }

    #[test]
    fn global_override_precedes_default() {
        let home = tempdir().unwrap();
        fs::write(home.path().join("AGENTS.md"), "default").unwrap();
        fs::write(home.path().join("AGENTS.override.md"), " override \n").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path())).as_deref(),
            Some("override")
        );
    }

    #[test]
    fn empty_global_override_falls_back_to_default() {
        let home = tempdir().unwrap();
        fs::write(home.path().join("AGENTS.override.md"), " \n\t").unwrap();
        fs::write(home.path().join("AGENTS.md"), " default \n").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path())).as_deref(),
            Some("default")
        );
    }

    #[test]
    fn global_directory_falls_back_to_default() {
        let home = tempdir().unwrap();
        fs::create_dir(home.path().join("AGENTS.override.md")).unwrap();
        fs::write(home.path().join("AGENTS.md"), "default").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path())).as_deref(),
            Some("default")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_global_override_falls_back_to_default() {
        use std::os::unix::fs::symlink;

        let home = tempdir().unwrap();
        symlink("AGENTS.override.md", home.path().join("AGENTS.override.md")).unwrap();
        fs::write(home.path().join("AGENTS.md"), "default").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path())).as_deref(),
            Some("default")
        );
    }

    #[test]
    fn global_instructions_decode_invalid_utf8_lossily() {
        let home = tempdir().unwrap();
        fs::write(home.path().join("AGENTS.md"), b"global\xff doc").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path())).as_deref(),
            Some("global\u{fffd} doc")
        );
    }

    #[test]
    fn global_instructions_precede_project_hierarchy() {
        let repo = tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        fs::write(repo.path().join("AGENTS.md"), "root").unwrap();
        let workspace = repo.path().join("crate/src");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(repo.path().join("crate/AGENTS.md"), "crate").unwrap();
        fs::write(workspace.join("AGENTS.override.md"), "local").unwrap();

        assert_eq!(
            load_instructions(&workspace, Some("global")).unwrap(),
            Some("global\n\n--- project-doc ---\n\nroot\n\ncrate\n\nlocal".to_owned())
        );
    }
}
