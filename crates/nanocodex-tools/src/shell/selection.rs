use std::{
    env,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use nix::unistd::{Uid, User};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellType {
    Zsh,
    Bash,
    PowerShell,
    Sh,
    Cmd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shell {
    shell_type: ShellType,
    path: PathBuf,
}

impl Shell {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) const fn name(&self) -> &'static str {
        match self.shell_type {
            ShellType::Zsh => "zsh",
            ShellType::Bash => "bash",
            ShellType::PowerShell => "powershell",
            ShellType::Sh => "sh",
            ShellType::Cmd => "cmd",
        }
    }

    pub(super) fn args(&self, script: &str, login: bool) -> Vec<String> {
        match self.shell_type {
            ShellType::Zsh | ShellType::Bash | ShellType::Sh => vec![
                if login { "-lc" } else { "-c" }.to_owned(),
                script.to_owned(),
            ],
            ShellType::PowerShell => {
                let mut args = Vec::with_capacity(3);
                if !login {
                    args.push("-NoProfile".to_owned());
                }
                args.push("-Command".to_owned());
                args.push(script.to_owned());
                args
            }
            ShellType::Cmd => vec!["/c".to_owned(), script.to_owned()],
        }
    }
}

fn detect_shell_type(shell_path: &Path) -> Option<ShellType> {
    match shell_path.as_os_str().to_str() {
        Some("zsh") => Some(ShellType::Zsh),
        Some("bash") => Some(ShellType::Bash),
        Some("pwsh" | "powershell") => Some(ShellType::PowerShell),
        Some("sh") => Some(ShellType::Sh),
        Some("cmd") => Some(ShellType::Cmd),
        _ => {
            let shell_name = shell_path.file_stem()?;
            let shell_name = Path::new(shell_name);
            (shell_name != shell_path)
                .then(|| detect_shell_type(shell_name))
                .flatten()
        }
    }
}

fn user_shell_path() -> Option<PathBuf> {
    User::from_uid(Uid::current())
        .ok()
        .flatten()
        .map(|user| user.shell)
}

fn file_exists(path: &Path) -> Option<PathBuf> {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file())
        .then(|| path.to_path_buf())
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(binary))
        .find(|candidate| {
            candidate.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

fn shell_path(
    shell_type: ShellType,
    provided_path: Option<&Path>,
    binary_name: &str,
    fallback_paths: &[&str],
) -> Option<PathBuf> {
    if let Some(path) = provided_path.and_then(file_exists) {
        return Some(path);
    }

    let user_shell = user_shell_path()
        .filter(|path| detect_shell_type(path) == Some(shell_type))
        .and_then(|path| file_exists(&path));
    if let Some(path) = user_shell {
        return Some(path);
    }

    find_in_path(binary_name).or_else(|| {
        fallback_paths
            .iter()
            .find_map(|path| file_exists(Path::new(path)))
    })
}

fn shell(shell_type: ShellType, provided_path: Option<&Path>) -> Option<Shell> {
    let path = match shell_type {
        ShellType::Zsh => shell_path(shell_type, provided_path, "zsh", &["/bin/zsh"]),
        ShellType::Bash => shell_path(
            shell_type,
            provided_path,
            "bash",
            &["/bin/bash", "/usr/bin/bash"],
        ),
        ShellType::PowerShell => {
            shell_path(shell_type, provided_path, "pwsh", &["/usr/local/bin/pwsh"])
                .or_else(|| shell_path(shell_type, provided_path, "powershell", &[]))
        }
        ShellType::Sh => shell_path(shell_type, provided_path, "sh", &["/bin/sh"]),
        ShellType::Cmd => shell_path(shell_type, provided_path, "cmd", &[]),
    }?;
    Some(Shell { shell_type, path })
}

fn ultimate_fallback_shell() -> Shell {
    Shell {
        shell_type: ShellType::Sh,
        path: PathBuf::from("/bin/sh"),
    }
}

pub(super) fn get_shell_by_model_provided_path(path: &str) -> Shell {
    let path = Path::new(path);
    detect_shell_type(path)
        .and_then(|shell_type| shell(shell_type, Some(path)))
        .unwrap_or_else(ultimate_fallback_shell)
}

pub(super) fn default_user_shell() -> Shell {
    let user_shell_path = user_shell_path();
    default_user_shell_from_path(user_shell_path.as_deref())
}

fn default_user_shell_from_path(user_shell_path: Option<&Path>) -> Shell {
    let user_default_shell = user_shell_path
        .and_then(detect_shell_type)
        .and_then(|shell_type| shell(shell_type, None));

    let shell_with_fallback = if cfg!(target_os = "macos") {
        user_default_shell
            .or_else(|| shell(ShellType::Zsh, None))
            .or_else(|| shell(ShellType::Bash, None))
    } else {
        user_default_shell
            .or_else(|| shell(ShellType::Bash, None))
            .or_else(|| shell(ShellType::Zsh, None))
    };

    shell_with_fallback.unwrap_or_else(ultimate_fallback_shell)
}

#[cfg(test)]
mod tests {
    use super::{
        ShellType, default_user_shell_from_path, detect_shell_type,
        get_shell_by_model_provided_path,
    };
    use std::path::Path;

    #[test]
    fn detects_codex_shell_names_and_paths() {
        assert_eq!(detect_shell_type(Path::new("zsh")), Some(ShellType::Zsh));
        assert_eq!(
            detect_shell_type(Path::new("/usr/bin/bash")),
            Some(ShellType::Bash)
        );
        assert_eq!(
            detect_shell_type(Path::new("pwsh.exe")),
            Some(ShellType::PowerShell)
        );
        assert_eq!(detect_shell_type(Path::new("fish")), None);
    }

    #[test]
    fn unavailable_user_shell_uses_codex_platform_fallbacks() {
        let shell = default_user_shell_from_path(Some(Path::new("/missing/fish")));
        if cfg!(target_os = "macos") && Path::new("/bin/zsh").is_file() {
            assert_eq!(shell.shell_type, ShellType::Zsh);
        } else if Path::new("/bin/bash").is_file() {
            assert_eq!(shell.shell_type, ShellType::Bash);
        } else {
            assert_eq!(shell.shell_type, ShellType::Sh);
        }
    }

    #[test]
    fn unknown_model_shell_falls_back_to_sh() {
        let shell = get_shell_by_model_provided_path("/definitely/missing/fish");
        assert_eq!(shell.shell_type, ShellType::Sh);
        assert_eq!(shell.path, Path::new("/bin/sh"));
    }
}
