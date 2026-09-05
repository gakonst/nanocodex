// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! External-editor handoff for the local prompt draft.

use std::{
    env, fs,
    path::Path,
    process::{ExitStatus, Stdio},
};
use tempfile::Builder;
use tokio::process::Command;

#[derive(Debug)]
pub(crate) enum EditorOutcome {
    Updated(String),
    Unchanged,
}

pub(crate) async fn edit(seed: &str, workspace: &Path) -> Result<EditorOutcome, String> {
    let editor = resolve_editor_command()?;
    edit_with(seed, workspace, &editor).await
}

fn resolve_editor_command() -> Result<Vec<String>, String> {
    match env::var("EDITOR") {
        Ok(editor) => resolve_editor_command_from(Some(&editor)),
        Err(env::VarError::NotPresent) => resolve_editor_command_from(None),
        Err(error) => Err(format!("$EDITOR is unavailable: {error}")),
    }
}

fn resolve_editor_command_from(editor: Option<&str>) -> Result<Vec<String>, String> {
    #[cfg(not(windows))]
    const DEFAULT_EDITOR: &str = "vi";
    #[cfg(windows)]
    const DEFAULT_EDITOR: &str = "notepad";

    parse_editor_command(editor.unwrap_or(DEFAULT_EDITOR))
}

#[cfg(not(windows))]
fn parse_editor_command(raw: &str) -> Result<Vec<String>, String> {
    let command =
        shlex::split(raw).ok_or_else(|| format!("failed to parse $EDITOR value `{raw}`"))?;
    if command.is_empty() {
        return Err(format!("failed to parse $EDITOR value `{raw}`"));
    }
    Ok(command)
}

#[cfg(windows)]
fn parse_editor_command(raw: &str) -> Result<Vec<String>, String> {
    if raw.trim().is_empty() {
        return Err(format!("failed to parse $EDITOR value `{raw}`"));
    }
    Ok(vec![raw.to_owned()])
}

async fn edit_with(
    seed: &str,
    workspace: &Path,
    editor: &[String],
) -> Result<EditorOutcome, String> {
    let draft = Builder::new()
        .prefix("nanocodex2-draft-")
        .suffix(".md")
        .tempfile()
        .map_err(|error| format!("failed to create temporary draft: {error}"))?;
    fs::write(draft.path(), seed)
        .map_err(|error| format!("failed to write temporary draft: {error}"))?;

    let status = launch(editor, draft.path(), workspace).await?;
    if !status.success() {
        return Ok(EditorOutcome::Unchanged);
    }

    let draft = fs::read_to_string(draft.path())
        .map_err(|error| format!("failed to read temporary draft: {error}"))?;
    let draft = remove_one_trailing_newline(draft);
    if draft == seed {
        Ok(EditorOutcome::Unchanged)
    } else {
        Ok(EditorOutcome::Updated(draft))
    }
}

async fn launch(editor: &[String], path: &Path, workspace: &Path) -> Result<ExitStatus, String> {
    let (program, arguments) = editor
        .split_first()
        .ok_or_else(|| "failed to parse an empty $EDITOR command".to_owned())?;
    Command::new(program)
        .args(arguments)
        .arg(path)
        .current_dir(workspace)
        .env("NANOCODEX2_EXTERNAL_EDITOR", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|error| format!("failed to launch `{program}`: {error}"))
}

fn remove_one_trailing_newline(mut draft: String) -> String {
    if draft.ends_with("\r\n") {
        draft.truncate(draft.len() - 2);
    } else if draft.ends_with('\n') {
        draft.pop();
    }
    draft
}

#[cfg(test)]
mod tests {
    use super::{EditorOutcome, edit_with, parse_editor_command, remove_one_trailing_newline};
    use std::path::Path;

    #[test]
    #[cfg(not(windows))]
    fn editor_arguments_preserve_shell_quoting() {
        assert_eq!(
            parse_editor_command("nvim -c 'set spell'").unwrap(),
            ["nvim", "-c", "set spell"]
        );
        assert!(parse_editor_command("nvim '").is_err());
        assert!(parse_editor_command("  ").is_err());
    }

    #[test]
    fn exactly_one_trailing_newline_is_removed() {
        assert_eq!(
            remove_one_trailing_newline("draft\n\n".to_owned()),
            "draft\n"
        );
        assert_eq!(remove_one_trailing_newline("draft\r\n".to_owned()), "draft");
        assert_eq!(remove_one_trailing_newline("draft".to_owned()), "draft");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn successful_editor_replaces_the_draft() {
        let command = [
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "test \"$NANOCODEX2_EXTERNAL_EDITOR\" = 1 && printf 'edited\\n' > \"$1\"".to_owned(),
            "nanocodex2-editor".to_owned(),
        ];

        let outcome = edit_with("seed", Path::new("."), &command).await.unwrap();
        assert!(matches!(outcome, EditorOutcome::Updated(draft) if draft == "edited"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn failed_editor_preserves_the_draft() {
        let command = ["/bin/sh".to_owned(), "-c".to_owned(), "exit 2".to_owned()];

        let outcome = edit_with("seed", Path::new("."), &command).await.unwrap();
        assert!(matches!(outcome, EditorOutcome::Unchanged));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn successful_noop_editor_preserves_the_draft() {
        let command = ["/bin/sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];

        let outcome = edit_with("seed", Path::new("."), &command).await.unwrap();
        assert!(matches!(outcome, EditorOutcome::Unchanged));
    }
}
