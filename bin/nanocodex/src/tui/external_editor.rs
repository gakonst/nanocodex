use std::{env, fs, path::Path, process::Stdio};

use eyre::{Result, WrapErr, eyre};
use tempfile::Builder;
use tokio::process::Command;

pub(super) fn resolve_editor_command() -> Result<Vec<String>> {
    let raw = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .wrap_err("neither $VISUAL nor $EDITOR is set")?;
    parse_editor_command(&raw)
}

#[cfg(not(windows))]
fn parse_editor_command(raw: &str) -> Result<Vec<String>> {
    let command = shlex::split(raw).ok_or_else(|| eyre!("failed to parse editor command"))?;
    if command.is_empty() {
        return Err(eyre!("editor command is empty"));
    }
    Ok(command)
}

#[cfg(windows)]
fn parse_editor_command(raw: &str) -> Result<Vec<String>> {
    if raw.trim().is_empty() {
        return Err(eyre!("editor command is empty"));
    }
    Ok(vec![raw.to_owned()])
}

pub(super) async fn edit(seed: &str, editor: &[String], cwd: &Path) -> Result<String> {
    let (program, arguments) = editor
        .split_first()
        .ok_or_else(|| eyre!("editor command is empty"))?;
    let path = Builder::new()
        .prefix("nanocodex-draft-")
        .suffix(".md")
        .tempfile()
        .wrap_err("failed to create editor draft")?
        .into_temp_path();
    fs::write(&path, seed).wrap_err("failed to write editor draft")?;

    let status = Command::new(program)
        .args(arguments)
        .arg(&path)
        .current_dir(cwd)
        .env("NANOCODEX_EXTERNAL_EDITOR", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .wrap_err("failed to launch external editor")?;
    if !status.success() {
        return Err(eyre!("external editor exited with {status}"));
    }

    fs::read_to_string(&path).wrap_err("failed to read editor draft")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{edit, parse_editor_command};

    #[test]
    #[cfg(not(windows))]
    fn editor_command_preserves_quoted_arguments() {
        assert_eq!(
            parse_editor_command("nvim -c 'set spell'").unwrap(),
            ["nvim", "-c", "set spell"]
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn editor_receives_the_draft_path_and_returns_edited_text() {
        let command = [
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "test \"$NANOCODEX_EXTERNAL_EDITOR\" = 1 && printf edited > \"$1\"".to_owned(),
            "nanocodex-editor".to_owned(),
        ];

        assert_eq!(
            edit("seed", &command, Path::new(".")).await.unwrap(),
            "edited"
        );
    }
}
