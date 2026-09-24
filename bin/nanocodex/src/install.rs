//! Native installation entrypoint used by the tiny curl bootstrap.

use std::{
    fs::{self, OpenOptions},
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
};

use clap::Args;
use eyre::{Context, Result, bail};

#[derive(Args, Debug)]
pub(crate) struct Install {
    /// Install the binaries without starting the guided login and Hand setup.
    #[arg(long)]
    no_setup: bool,

    /// Do not add the Nanocodex bin directory to the detected shell profile.
    #[arg(long)]
    no_modify_path: bool,
}

#[derive(Debug, PartialEq)]
struct ProfileEdit {
    path: PathBuf,
    line: String,
}

impl Install {
    pub(crate) async fn run(self) -> Result<()> {
        eprintln!("Installing the latest verified Nanocodex release…");
        let root = crate::update::install_latest().await?;
        let bin = root.join("bin");
        let executable = bin.join(if cfg!(windows) {
            "nanocodex.exe"
        } else {
            "nanocodex"
        });

        if !self.no_modify_path && !path_contains(&bin) {
            match profile_edit(&bin) {
                Ok(Some(edit)) => match append_profile(&edit) {
                    Ok(true) => {
                        eprintln!("Added {} to PATH in {}", bin.display(), edit.path.display())
                    }
                    Ok(false) => {}
                    Err(error) => eprintln!(
                        "Could not update {} ({error:#}). Add this line yourself:\n  {}",
                        edit.path.display(),
                        edit.line
                    ),
                },
                Ok(None) => eprintln!("Add {} to PATH, then restart your shell.", bin.display()),
                Err(error) => eprintln!("Could not prepare PATH integration: {error:#}"),
            }
        }

        println!("Nanocodex is installed at {}", executable.display());
        let no_setup =
            self.no_setup || std::env::var("NANOCODEX_INSTALL_NO_SETUP").as_deref() == Ok("1");
        if no_setup {
            return Ok(());
        }
        if !std::io::stdin().is_terminal() {
            println!(
                "No interactive terminal detected. Finish setup with: {} setup",
                executable.display()
            );
            return Ok(());
        }

        let mut command = tokio::process::Command::new(&executable);
        command
            .args(["setup", "--refresh"])
            .env("NANOCODEX_DIR", &root)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if std::env::var_os("NANOCODEX_COMPUTER").is_some_and(|value| !value.is_empty()) {
            command.arg("--skip-computer");
        }
        let status = command
            .status()
            .await
            .wrap_err("failed to start the installed Nanocodex setup")?;
        if !status.success() {
            bail!(
                "the binaries are installed; resume setup with: {} setup",
                executable.display()
            );
        }
        Ok(())
    }
}

fn path_contains(bin: &Path) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|entry| entry == bin))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn profile_edit(bin: &Path) -> Result<Option<ProfileEdit>> {
    #[cfg(windows)]
    {
        let _ = bin;
        return Ok(None);
    }
    #[cfg(not(windows))]
    {
        let home =
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is not set"))?);
        let shell = std::env::var_os("SHELL")
            .and_then(|shell| PathBuf::from(shell).file_name().map(|name| name.to_owned()));
        let quoted = shell_quote(
            bin.to_str()
                .ok_or_else(|| eyre::eyre!("installation path is not valid UTF-8"))?,
        );
        let (path, line) = match shell.as_deref().and_then(|name| name.to_str()) {
            Some("zsh") => {
                let directory = std::env::var_os("ZDOTDIR").map_or(home, PathBuf::from);
                (
                    directory.join(".zshenv"),
                    format!("export PATH={quoted}:\"$PATH\""),
                )
            }
            Some("bash") => (
                home.join(".bashrc"),
                format!("export PATH={quoted}:\"$PATH\""),
            ),
            Some("fish") => {
                let directory = std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"));
                (
                    directory.join("fish/config.fish"),
                    format!("fish_add_path -- {quoted}"),
                )
            }
            Some("sh" | "ash" | "dash" | "ksh") => (
                home.join(".profile"),
                format!("export PATH={quoted}:\"$PATH\""),
            ),
            _ => return Ok(None),
        };
        Ok(Some(ProfileEdit { path, line }))
    }
}

fn append_profile(edit: &ProfileEdit) -> Result<bool> {
    if let Ok(metadata) = fs::metadata(&edit.path)
        && !metadata.is_file()
    {
        bail!("shell profile is not a regular file");
    }
    let mut existing = String::new();
    match fs::File::open(&edit.path) {
        Ok(mut profile) => {
            profile.read_to_string(&mut existing)?;
            if existing.contains(&edit.line) {
                return Ok(false);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = edit.path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut profile = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&edit.path)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        profile.write_all(b"\n")?;
    }
    writeln!(profile, "\n# Nanocodex\n{}", edit.line)?;
    profile.sync_all()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_paths_are_quoted_as_data() {
        assert_eq!(shell_quote("/tmp/a b'c"), "'/tmp/a b'\"'\"'c'");
    }

    #[test]
    fn profile_edits_are_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let edit = ProfileEdit {
            path: directory.path().join("profile"),
            line: "export PATH='/tmp/ncx':\"$PATH\"".into(),
        };
        assert!(append_profile(&edit).unwrap());
        assert!(!append_profile(&edit).unwrap());
        assert_eq!(
            fs::read_to_string(edit.path).unwrap(),
            "\n# Nanocodex\nexport PATH='/tmp/ncx':\"$PATH\"\n"
        );
    }
}
