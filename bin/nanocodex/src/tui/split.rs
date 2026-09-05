use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use eyre::{Result, WrapErr, eyre};

pub(super) struct PreparedSplit {
    backend: Backend,
    executable: PathBuf,
    cwd: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Backend {
    Tmux { pane: Option<OsString> },
    Zellij { pane: Option<OsString> },
    WezTermPane { pane: OsString },
    Iterm,
    MacKitty,
    MacTerminal,
    WindowsTerminalPane,
    WindowsTerminalWindow,
    Kitty,
    Ghostty,
    WezTermWindow,
    Alacritty,
    GnomeTerminal,
    Konsole,
    XfceTerminal,
    Foot,
    FootClient,
    Xterm,
    SystemTerminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Platform {
    Macos,
    Linux,
    Windows,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LaunchMode {
    Wait,
    Spawn,
}

#[derive(Debug, Eq, PartialEq)]
struct CommandSpec {
    program: OsString,
    arguments: Vec<OsString>,
    mode: LaunchMode,
}

struct Detector<'a> {
    platform: Platform,
    env: &'a dyn Fn(&str) -> Option<OsString>,
    available: &'a dyn Fn(&str) -> bool,
}

impl PreparedSplit {
    pub(super) fn detect(cwd: &Path) -> Result<Self> {
        let executable =
            env::current_exe().wrap_err("failed to resolve the nanocodex executable")?;
        let detector = Detector {
            platform: Platform::current(),
            env: &|name| env::var_os(name),
            available: &program_available,
        };
        let backend = detector
            .detect()
            .map_err(|error| eyre!("terminal split is unavailable: {error}"))?;
        Ok(Self {
            backend,
            executable,
            cwd: cwd.to_path_buf(),
        })
    }

    pub(super) fn launch(&self, thread_id: &str) -> Result<&'static str> {
        let command = self.command(thread_id)?;
        run_command(&command, &self.cwd).wrap_err_with(|| {
            format!(
                "failed to open {} for thread {thread_id}",
                self.backend.destination()
            )
        })?;
        if let Some(command) = self.backend.refocus_command()
            && let Err(error) = run_command(&command, &self.cwd)
        {
            tracing::warn!(%error, "opened split but could not restore focus to the main pane");
        }
        Ok(self.backend.destination())
    }

    fn command(&self, thread_id: &str) -> Result<CommandSpec> {
        self.backend.command(&self.executable, &self.cwd, thread_id)
    }
}

impl Backend {
    const fn destination(&self) -> &'static str {
        match self {
            Self::Tmux { .. } => "the right tmux pane",
            Self::Zellij { .. } => "the right Zellij pane",
            Self::WezTermPane { .. } => "the right WezTerm pane",
            Self::Iterm => "the right iTerm2 pane",
            Self::WindowsTerminalPane => "the right Windows Terminal pane",
            Self::MacKitty => "a new kitty window",
            Self::MacTerminal => "a new Terminal window",
            Self::WindowsTerminalWindow => "a new Windows Terminal window",
            Self::Kitty => "a new kitty window",
            Self::Ghostty => "a new Ghostty window",
            Self::WezTermWindow => "a new WezTerm window",
            Self::Alacritty => "a new Alacritty window",
            Self::GnomeTerminal => "a new GNOME Terminal window",
            Self::Konsole => "a new Konsole window",
            Self::XfceTerminal => "a new Xfce Terminal window",
            Self::Foot => "a new foot window",
            Self::FootClient => "a new foot window",
            Self::Xterm => "a new xterm window",
            Self::SystemTerminal => "a new terminal window",
        }
    }

    fn command(&self, executable: &Path, cwd: &Path, thread_id: &str) -> Result<CommandSpec> {
        let direct_resume = || {
            [
                executable.as_os_str().to_owned(),
                OsString::from("resume"),
                OsString::from(thread_id),
            ]
        };
        let spec = match self {
            Self::Tmux { pane } => {
                let mut arguments = os_args(["split-window", "-d", "-h", "-c"]);
                arguments.push(cwd.as_os_str().to_owned());
                if let Some(pane) = pane {
                    arguments.extend([OsString::from("-t"), pane.clone()]);
                }
                arguments.push(resume_shell_command(executable, None, thread_id)?);
                CommandSpec::wait("tmux", arguments)
            }
            Self::Zellij { .. } => {
                let mut arguments = os_args([
                    "action",
                    "new-pane",
                    "--direction",
                    "right",
                    "--close-on-exit",
                    "--cwd",
                ]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.push(OsString::from("--"));
                arguments.extend(direct_resume());
                CommandSpec::wait("zellij", arguments)
            }
            Self::WezTermPane { pane } => {
                let mut arguments = os_args(["cli", "split-pane", "--right", "--pane-id"]);
                arguments.push(pane.clone());
                arguments.push(OsString::from("--cwd"));
                arguments.push(cwd.as_os_str().to_owned());
                arguments.push(OsString::from("--"));
                arguments.extend(direct_resume());
                CommandSpec::wait("wezterm", arguments)
            }
            Self::Iterm => CommandSpec::wait(
                "/usr/bin/osascript",
                vec![
                    OsString::from("-e"),
                    OsString::from(ITERM_SPLIT_SCRIPT),
                    resume_shell_command(executable, Some(cwd), thread_id)?,
                ],
            ),
            Self::MacKitty => {
                let mut arguments = os_args(["-na", "kitty.app", "--args", "--directory"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.extend(direct_resume());
                CommandSpec::wait("open", arguments)
            }
            Self::MacTerminal => CommandSpec::wait(
                "/usr/bin/osascript",
                vec![
                    OsString::from("-e"),
                    OsString::from(MAC_TERMINAL_SCRIPT),
                    resume_shell_command(executable, Some(cwd), thread_id)?,
                ],
            ),
            Self::WindowsTerminalPane => {
                let mut arguments = os_args(["-w", "0", "split-pane", "-V", "-d"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.extend(direct_resume());
                CommandSpec::wait("wt.exe", arguments)
            }
            Self::WindowsTerminalWindow => {
                let mut arguments = os_args(["-w", "new", "new-tab", "-d"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.extend(direct_resume());
                CommandSpec::wait("wt.exe", arguments)
            }
            Self::Kitty => {
                let mut arguments = os_args(["--detach", "--directory"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.extend(direct_resume());
                CommandSpec::wait("kitty", arguments)
            }
            Self::Ghostty => {
                let mut arguments = vec![OsString::from(format!(
                    "--working-directory={}",
                    utf8_path(cwd, "workspace")?
                ))];
                arguments.push(OsString::from("-e"));
                arguments.extend(direct_resume());
                CommandSpec::spawn("ghostty", arguments)
            }
            Self::WezTermWindow => {
                let mut arguments = os_args(["start", "--cwd"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.push(OsString::from("--"));
                arguments.extend(direct_resume());
                CommandSpec::spawn("wezterm", arguments)
            }
            Self::Alacritty => {
                let mut arguments = os_args(["--working-directory"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.push(OsString::from("-e"));
                arguments.extend(direct_resume());
                CommandSpec::spawn("alacritty", arguments)
            }
            Self::GnomeTerminal => {
                let mut arguments = vec![OsString::from(format!(
                    "--working-directory={}",
                    utf8_path(cwd, "workspace")?
                ))];
                arguments.push(OsString::from("--"));
                arguments.extend(direct_resume());
                CommandSpec::spawn("gnome-terminal", arguments)
            }
            Self::Konsole => {
                let mut arguments = os_args(["--workdir"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.push(OsString::from("-e"));
                arguments.extend(direct_resume());
                CommandSpec::spawn("konsole", arguments)
            }
            Self::XfceTerminal => {
                let command = resume_shell_command(executable, None, thread_id)?;
                let mut command_argument = OsString::from("--command=");
                command_argument.push(command);
                let arguments = vec![
                    OsString::from(format!(
                        "--working-directory={}",
                        utf8_path(cwd, "workspace")?
                    )),
                    command_argument,
                ];
                CommandSpec::spawn("xfce4-terminal", arguments)
            }
            Self::Foot => {
                let mut arguments = os_args(["--working-directory"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.extend(direct_resume());
                CommandSpec::spawn("foot", arguments)
            }
            Self::FootClient => {
                let mut arguments = os_args(["--working-directory"]);
                arguments.push(cwd.as_os_str().to_owned());
                arguments.extend(direct_resume());
                CommandSpec::spawn("footclient", arguments)
            }
            Self::Xterm => {
                let mut arguments = os_args(["-e"]);
                arguments.extend(direct_resume());
                CommandSpec::spawn("xterm", arguments)
            }
            Self::SystemTerminal => {
                let mut arguments = os_args(["-e"]);
                arguments.extend(direct_resume());
                CommandSpec::spawn("x-terminal-emulator", arguments)
            }
        };
        Ok(spec)
    }

    fn refocus_command(&self) -> Option<CommandSpec> {
        match self {
            Self::Zellij { pane: Some(pane) } => Some(CommandSpec::wait(
                "zellij",
                vec![
                    OsString::from("action"),
                    OsString::from("focus-pane-id"),
                    pane.clone(),
                ],
            )),
            Self::WezTermPane { pane } => Some(CommandSpec::wait(
                "wezterm",
                vec![
                    OsString::from("cli"),
                    OsString::from("activate-pane"),
                    OsString::from("--pane-id"),
                    pane.clone(),
                ],
            )),
            _ => None,
        }
    }
}

impl Platform {
    const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

impl CommandSpec {
    fn wait(program: impl Into<OsString>, arguments: Vec<OsString>) -> Self {
        Self {
            program: program.into(),
            arguments,
            mode: LaunchMode::Wait,
        }
    }

    fn spawn(program: impl Into<OsString>, arguments: Vec<OsString>) -> Self {
        Self {
            program: program.into(),
            arguments,
            mode: LaunchMode::Spawn,
        }
    }
}

impl Detector<'_> {
    fn detect(&self) -> std::result::Result<Backend, String> {
        if self.has_env("TMUX") && self.has_program("tmux") {
            return Ok(Backend::Tmux {
                pane: self.nonempty_env("TMUX_PANE"),
            });
        }
        if (self.has_env("ZELLIJ") || self.has_env("ZELLIJ_SESSION_NAME"))
            && self.has_program("zellij")
        {
            return Ok(Backend::Zellij {
                pane: self.nonempty_env("ZELLIJ_PANE_ID"),
            });
        }
        if self.is_remote() {
            return Err(
                "no active tmux or Zellij session can create a sibling terminal over SSH; run `nanocodex resume <thread-id>` in another terminal"
                    .to_owned(),
            );
        }
        if let Some(pane) = self.nonempty_env("WEZTERM_PANE")
            && self.has_program("wezterm")
        {
            return Ok(Backend::WezTermPane { pane });
        }
        if self.platform == Platform::Windows {
            return if self.has_program("wt.exe") {
                Ok(if self.has_env("WT_SESSION") {
                    Backend::WindowsTerminalPane
                } else {
                    Backend::WindowsTerminalWindow
                })
            } else {
                Err("Windows Terminal (`wt.exe`) was not found on PATH".to_owned())
            };
        }

        let terminal = self.terminal_name();
        if self.platform == Platform::Macos {
            if terminal.contains("iterm") && self.has_program("/usr/bin/osascript") {
                return Ok(Backend::Iterm);
            }
            if terminal.contains("kitty") && self.has_program("open") {
                return Ok(Backend::MacKitty);
            }
            if let Some(backend) = self.detect_named_terminal(&terminal) {
                return Ok(backend);
            }
            return if self.has_program("/usr/bin/osascript") {
                Ok(Backend::MacTerminal)
            } else {
                Err("macOS Terminal automation (`osascript`) was not found".to_owned())
            };
        }
        if self.platform != Platform::Linux {
            return Err("this operating system has no terminal launcher".to_owned());
        }
        if !self.has_env("DISPLAY") && !self.has_env("WAYLAND_DISPLAY") {
            return Err(
                "no graphical display or active terminal multiplexer is available; run `nanocodex resume <thread-id>` in another terminal"
                    .to_owned(),
            );
        }
        if let Some(backend) = self.detect_named_terminal(&terminal) {
            return Ok(backend);
        }
        [
            ("x-terminal-emulator", Backend::SystemTerminal),
            ("ghostty", Backend::Ghostty),
            ("kitty", Backend::Kitty),
            ("wezterm", Backend::WezTermWindow),
            ("alacritty", Backend::Alacritty),
            ("gnome-terminal", Backend::GnomeTerminal),
            ("konsole", Backend::Konsole),
            ("xfce4-terminal", Backend::XfceTerminal),
            ("footclient", Backend::FootClient),
            ("foot", Backend::Foot),
            ("xterm", Backend::Xterm),
        ]
        .into_iter()
        .find_map(|(program, backend)| self.has_program(program).then_some(backend))
        .ok_or_else(|| {
            "no supported terminal launcher was found on PATH; run `nanocodex resume <thread-id>` in another terminal"
                .to_owned()
        })
    }

    fn detect_named_terminal(&self, terminal: &str) -> Option<Backend> {
        [
            ("ghostty", "ghostty", Backend::Ghostty),
            ("kitty", "kitty", Backend::Kitty),
            ("wezterm", "wezterm", Backend::WezTermWindow),
            ("alacritty", "alacritty", Backend::Alacritty),
            ("gnome", "gnome-terminal", Backend::GnomeTerminal),
            ("konsole", "konsole", Backend::Konsole),
            ("xfce", "xfce4-terminal", Backend::XfceTerminal),
            ("foot", "footclient", Backend::FootClient),
            ("foot", "foot", Backend::Foot),
            ("xterm", "xterm", Backend::Xterm),
        ]
        .into_iter()
        .find_map(|(needle, program, backend)| {
            (terminal.contains(needle) && self.has_program(program)).then_some(backend)
        })
    }

    fn terminal_name(&self) -> String {
        ["TERM_PROGRAM", "TERMINAL_EMULATOR", "COLORTERM", "TERM"]
            .into_iter()
            .filter_map(|name| self.nonempty_env(name))
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn is_remote(&self) -> bool {
        self.has_env("SSH_TTY") || self.has_env("SSH_CONNECTION") || self.has_env("SSH_CLIENT")
    }

    fn has_program(&self, program: &str) -> bool {
        (self.available)(program)
    }

    fn has_env(&self, name: &str) -> bool {
        self.nonempty_env(name).is_some()
    }

    fn nonempty_env(&self, name: &str) -> Option<OsString> {
        (self.env)(name).filter(|value| !value.is_empty())
    }
}

fn run_command(spec: &CommandSpec, cwd: &Path) -> Result<()> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    match spec.mode {
        LaunchMode::Spawn => {
            command
                .stderr(Stdio::null())
                .spawn()
                .wrap_err_with(|| format!("could not start {}", spec.program.to_string_lossy()))?;
            Ok(())
        }
        LaunchMode::Wait => {
            let output = command
                .stderr(Stdio::piped())
                .output()
                .wrap_err_with(|| format!("could not run {}", spec.program.to_string_lossy()))?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if stderr.is_empty() {
                Err(eyre!(
                    "{} exited with {}",
                    spec.program.to_string_lossy(),
                    output.status
                ))
            } else {
                Err(eyre!("{} failed: {stderr}", spec.program.to_string_lossy()))
            }
        }
    }
}

fn resume_shell_command(
    executable: &Path,
    cwd: Option<&Path>,
    thread_id: &str,
) -> Result<OsString> {
    let executable = quote_shell_path(executable, "nanocodex executable")?;
    let thread_id = shlex::try_quote(thread_id)
        .map_err(|error| eyre!("thread ID cannot be represented in a shell command: {error}"))?;
    let resume = format!("exec {executable} resume {thread_id}");
    let command = if let Some(cwd) = cwd {
        let cwd = quote_shell_path(cwd, "workspace")?;
        format!("cd {cwd} && {resume}")
    } else {
        resume
    };
    Ok(OsString::from(command))
}

fn quote_shell_path(path: &Path, label: &str) -> Result<String> {
    let path = utf8_path(path, label)?;
    shlex::try_quote(path)
        .map(Into::into)
        .map_err(|error| eyre!("{label} cannot be represented in a shell command: {error}"))
}

fn utf8_path<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    path.to_str()
        .ok_or_else(|| eyre!("{label} path is not valid UTF-8: {}", path.display()))
}

fn os_args<const N: usize>(arguments: [&str; N]) -> Vec<OsString> {
    arguments.into_iter().map(OsString::from).collect()
}

fn program_available(program: &str) -> bool {
    let program = Path::new(program);
    if program.components().count() > 1 {
        return executable_file(program);
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|directory| {
        if executable_file(&directory.join(program)) {
            return true;
        }
        #[cfg(windows)]
        {
            let extensions =
                env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
            return extensions.to_string_lossy().split(';').any(|extension| {
                executable_file(
                    &directory
                        .join(program)
                        .with_extension(extension.trim_start_matches('.')),
                )
            });
        }
        #[cfg(not(windows))]
        false
    })
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

const ITERM_SPLIT_SCRIPT: &str = r#"on run argv
tell application "iTerm2"
    tell current session of current window
        split vertically with same profile command (item 1 of argv)
    end tell
end tell
end run"#;

const MAC_TERMINAL_SCRIPT: &str = r#"on run argv
tell application "Terminal"
    activate
    do script (item 1 of argv)
end tell
end run"#;

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, ffi::OsString, path::Path};

    use super::{Backend, Detector, Platform, PreparedSplit};

    #[test]
    fn tmux_wins_and_targets_the_calling_pane() {
        let env = environment([("TMUX", "socket"), ("TMUX_PANE", "%7")]);
        let available = programs(["tmux", "zellij", "wezterm"]);

        assert_eq!(
            detect(Platform::Linux, &env, &available).unwrap(),
            Backend::Tmux {
                pane: Some(OsString::from("%7"))
            }
        );
    }

    #[test]
    fn zellij_is_the_next_multiplexer_and_retains_the_source_pane() {
        let env = environment([
            ("ZELLIJ", "0"),
            ("ZELLIJ_SESSION_NAME", "work"),
            ("ZELLIJ_PANE_ID", "3"),
        ]);
        let available = programs(["zellij"]);

        assert_eq!(
            detect(Platform::Linux, &env, &available).unwrap(),
            Backend::Zellij {
                pane: Some(OsString::from("3"))
            }
        );
    }

    #[test]
    fn wezterm_uses_a_right_split_before_graphical_fallbacks() {
        let env = environment([
            ("WEZTERM_PANE", "12"),
            ("TERM_PROGRAM", "WezTerm"),
            ("DISPLAY", ":0"),
        ]);
        let available = programs(["wezterm", "x-terminal-emulator"]);

        assert_eq!(
            detect(Platform::Linux, &env, &available).unwrap(),
            Backend::WezTermPane {
                pane: OsString::from("12")
            }
        );
    }

    #[test]
    fn linux_prefers_the_detected_terminal_then_the_system_default() {
        let env = environment([
            ("TERM_PROGRAM", "ghostty"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ]);
        let available = programs(["ghostty", "x-terminal-emulator"]);
        assert_eq!(
            detect(Platform::Linux, &env, &available).unwrap(),
            Backend::Ghostty
        );

        let env = environment([("TERM", "xterm-256color"), ("DISPLAY", ":0")]);
        let available = programs(["x-terminal-emulator"]);
        assert_eq!(
            detect(Platform::Linux, &env, &available).unwrap(),
            Backend::SystemTerminal
        );
    }

    #[test]
    fn macos_uses_iterm_split_then_default_terminal_automation() {
        let env = environment([("TERM_PROGRAM", "iTerm.app")]);
        let available = programs(["/usr/bin/osascript"]);
        assert_eq!(
            detect(Platform::Macos, &env, &available).unwrap(),
            Backend::Iterm
        );

        let env = environment([("TERM_PROGRAM", "WarpTerminal")]);
        assert_eq!(
            detect(Platform::Macos, &env, &available).unwrap(),
            Backend::MacTerminal
        );
    }

    #[test]
    fn remote_sessions_require_a_remote_multiplexer() {
        let env = environment([
            ("SSH_CONNECTION", "client server"),
            ("TERM_PROGRAM", "iTerm.app"),
        ]);
        let available = programs(["/usr/bin/osascript"]);

        let error = detect(Platform::Macos, &env, &available).unwrap_err();
        assert!(error.contains("over SSH"));
        assert!(error.contains("nanocodex resume"));
    }

    #[test]
    fn tmux_command_quotes_paths_and_keeps_focus_on_the_main_pane() {
        let split = PreparedSplit {
            backend: Backend::Tmux {
                pane: Some(OsString::from("%2")),
            },
            executable: "/Applications/Nano Codex/bin/nanocodex".into(),
            cwd: "/tmp/work tree".into(),
        };

        let command = split.command("0198-thread").unwrap();
        assert_eq!(command.program, "tmux");
        assert_eq!(
            command.arguments,
            [
                "split-window",
                "-d",
                "-h",
                "-c",
                "/tmp/work tree",
                "-t",
                "%2",
                "exec '/Applications/Nano Codex/bin/nanocodex' resume 0198-thread",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn zellij_and_wezterm_pass_resume_arguments_without_a_shell() {
        let zellij = PreparedSplit {
            backend: Backend::Zellij { pane: None },
            executable: "/opt/nanocodex".into(),
            cwd: "/work".into(),
        };
        assert_eq!(
            zellij.command("thread").unwrap().arguments,
            [
                "action",
                "new-pane",
                "--direction",
                "right",
                "--close-on-exit",
                "--cwd",
                "/work",
                "--",
                "/opt/nanocodex",
                "resume",
                "thread",
            ]
            .map(OsString::from)
        );

        let wezterm = PreparedSplit {
            backend: Backend::WezTermPane {
                pane: OsString::from("9"),
            },
            executable: "/opt/nanocodex".into(),
            cwd: "/work".into(),
        };
        assert_eq!(
            wezterm.command("thread").unwrap().arguments,
            [
                "cli",
                "split-pane",
                "--right",
                "--pane-id",
                "9",
                "--cwd",
                "/work",
                "--",
                "/opt/nanocodex",
                "resume",
                "thread",
            ]
            .map(OsString::from)
        );
    }

    fn detect(
        platform: Platform,
        environment: &HashMap<String, OsString>,
        available: &[String],
    ) -> Result<Backend, String> {
        Detector {
            platform,
            env: &|name| environment.get(name).cloned(),
            available: &|program| available.iter().any(|candidate| candidate == program),
        }
        .detect()
    }

    fn environment<const N: usize>(values: [(&str, &str); N]) -> HashMap<String, OsString> {
        values
            .into_iter()
            .map(|(name, value)| (name.to_owned(), OsString::from(value)))
            .collect()
    }

    fn programs<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn executable_probe_accepts_the_current_test_binary() {
        assert!(super::executable_file(
            std::env::current_exe().unwrap().as_path()
        ));
        assert!(!super::executable_file(Path::new("/definitely/missing")));
    }
}
