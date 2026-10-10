use std::{path::Path, sync::Arc};

use nanocodex_home::{AgentHome, Convention, Diagnostic, Level, ProjectHome};
use tracing::warn;

const PROJECT_DOC_SEPARATOR: &str = "\n\n--- project-doc ---\n\n";

/// User-level instructions from the configured homes: the Codex home's
/// `AGENTS.override.md` (when non-empty) or `AGENTS.md`, then the Claude
/// home's `CLAUDE.md` when it is a distinct document.
pub(crate) fn load_global_instructions(
    codex_home: Option<&Path>,
    claude_home: Option<&Path>,
) -> Option<Arc<str>> {
    let (codex, claude) = match (codex_home, claude_home) {
        (None, None) => return None,
        (Some(codex), Some(claude)) => (codex, claude),
        (Some(home), None) | (None, Some(home)) => (home, home),
    };
    let global = AgentHome::new(codex, claude).global_instructions();
    report(&global.diagnostics);
    // A home that was not configured contributes nothing, even when the
    // configured one is passed for both.
    let texts = global
        .sources
        .iter()
        .filter(|source| match source.convention {
            Convention::Codex => codex_home.is_some(),
            Convention::Claude => claude_home.is_some(),
        })
        .map(|source| source.text.as_str())
        .collect::<Vec<_>>();
    (!texts.is_empty()).then(|| Arc::from(texts.join("\n\n")))
}

pub(super) fn load_instructions(
    workspace: &Path,
    global_instructions: Option<&str>,
) -> Option<String> {
    combine_instructions(
        global_instructions,
        load_project_instructions(workspace).as_deref(),
    )
}

pub(super) fn combine_instructions(
    global_instructions: Option<&str>,
    project_instructions: Option<&str>,
) -> Option<String> {
    match (global_instructions, project_instructions) {
        (Some(global), Some(project)) => Some(format!("{global}{PROJECT_DOC_SEPARATOR}{project}")),
        (Some(global), None) => Some(global.to_owned()),
        (None, Some(project)) => Some(project.to_owned()),
        (None, None) => None,
    }
}

/// Project instructions root-to-leaf, each loaded whole: per directory
/// `AGENTS.override.md` or `AGENTS.md`, then the Claude conventions
/// `CLAUDE.md`, `CLAUDE.local.md`, and `.claude/CLAUDE.md`, each distinct
/// document once.
fn load_project_instructions(workspace: &Path) -> Option<String> {
    let instructions = ProjectHome::discover(workspace).read_instructions();
    report(&instructions.diagnostics);
    instructions.combined()
}

fn report(diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        if matches!(diagnostic.level, Level::Warning) {
            warn!(
                path = %diagnostic.path.display(),
                message = %diagnostic.message,
                "instruction file skipped"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn missing_global_files_return_no_instructions() {
        let home = tempdir().unwrap();

        assert!(load_global_instructions(Some(home.path()), None).is_none());
    }

    #[test]
    fn global_override_precedes_default() {
        let home = tempdir().unwrap();
        fs::write(home.path().join("AGENTS.md"), "default").unwrap();
        fs::write(home.path().join("AGENTS.override.md"), " override \n").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path()), None).as_deref(),
            Some("override")
        );
    }

    #[test]
    fn empty_global_override_falls_back_to_default() {
        let home = tempdir().unwrap();
        fs::write(home.path().join("AGENTS.override.md"), " \n\t").unwrap();
        fs::write(home.path().join("AGENTS.md"), " default \n").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path()), None).as_deref(),
            Some("default")
        );
    }

    #[test]
    fn global_directory_falls_back_to_default() {
        let home = tempdir().unwrap();
        fs::create_dir(home.path().join("AGENTS.override.md")).unwrap();
        fs::write(home.path().join("AGENTS.md"), "default").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path()), None).as_deref(),
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
            load_global_instructions(Some(home.path()), None).as_deref(),
            Some("default")
        );
    }

    #[test]
    fn global_instructions_decode_invalid_utf8_lossily() {
        let home = tempdir().unwrap();
        fs::write(home.path().join("AGENTS.md"), b"global\xff doc").unwrap();

        assert_eq!(
            load_global_instructions(Some(home.path()), None).as_deref(),
            Some("global\u{fffd} doc")
        );
    }

    #[test]
    fn claude_home_instructions_follow_the_codex_home() {
        let codex = tempdir().unwrap();
        let claude = tempdir().unwrap();
        fs::write(codex.path().join("AGENTS.md"), "codex").unwrap();
        fs::write(claude.path().join("CLAUDE.md"), " claude \n").unwrap();

        assert_eq!(
            load_global_instructions(Some(codex.path()), Some(claude.path())).as_deref(),
            Some("codex\n\nclaude")
        );
        assert_eq!(
            load_global_instructions(None, Some(claude.path())).as_deref(),
            Some("claude")
        );
        // An unconfigured Claude home is never read.
        assert_eq!(
            load_global_instructions(Some(claude.path()), None).as_deref(),
            None
        );
    }

    #[test]
    fn project_claude_conventions_join_agents_md_once() {
        let repo = tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        fs::write(repo.path().join("AGENTS.md"), "shared").unwrap();
        // A CLAUDE.md with the same text (or a symlink to AGENTS.md) is listed once.
        fs::write(repo.path().join("CLAUDE.md"), "shared").unwrap();
        fs::write(repo.path().join("CLAUDE.local.md"), "personal").unwrap();

        assert_eq!(
            load_instructions(repo.path(), None),
            Some("shared\n\npersonal".to_owned())
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
            load_instructions(&workspace, Some("global")),
            Some("global\n\n--- project-doc ---\n\nroot\n\ncrate\n\nlocal".to_owned())
        );
    }

    #[test]
    fn whitespace_project_docs_are_skipped() {
        let repo = tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        fs::write(repo.path().join("AGENTS.md"), " ".repeat(32 * 1024)).unwrap();
        let workspace = repo.path().join("crate");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("AGENTS.md"), "use the crate instructions").unwrap();

        assert_eq!(
            load_instructions(&workspace, None),
            Some("use the crate instructions".to_owned())
        );
    }

    #[test]
    fn project_docs_beyond_the_former_32_kib_budget_are_loaded_whole() {
        let repo = tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        let root = format!("root {}", "r".repeat(64 * 1024));
        fs::write(repo.path().join("AGENTS.md"), &root).unwrap();
        let workspace = repo.path().join("crate");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("AGENTS.md"), "crate tail").unwrap();

        assert_eq!(
            load_instructions(&workspace, None),
            Some(format!("{root}\n\ncrate tail"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_read_errors_preserve_global_instructions() {
        use std::os::unix::fs::symlink;

        let repo = tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        symlink("AGENTS.md", repo.path().join("AGENTS.md")).unwrap();

        assert_eq!(
            load_instructions(repo.path(), Some("global")),
            Some("global".to_owned())
        );
    }
}
