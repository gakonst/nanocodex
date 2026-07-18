use std::{
    env,
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
};

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use super::selection::Shell;

const SENSITIVE_ENV_PARTS: [&str; 11] = [
    "AUTH",
    "AUTHORIZATION",
    "COOKIE",
    "CREDENTIAL",
    "CREDENTIALS",
    "KEY",
    "PASS",
    "PASSWD",
    "PASSWORD",
    "SECRET",
    "TOKEN",
];

const NORMALIZED_ENVIRONMENT: [(&str, &str); 10] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CODEX_CI", "1"),
];

pub(super) struct SpawnedProcess {
    pub(super) child: ProcessChild,
    pub(super) stdin: Option<ProcessStdin>,
    pub(super) output: ProcessOutput,
    pub(super) process_group: ProcessGroupGuard,
}

pub(super) enum ProcessChild {
    Pipes(Child),
    Pty(JoinHandle<io::Result<i32>>),
}

impl ProcessChild {
    pub(super) async fn wait(&mut self) -> io::Result<i32> {
        match self {
            Self::Pipes(child) => child.wait().await.map(exit_code),
            Self::Pty(wait) => wait
                .await
                .map_err(|error| io::Error::other(format!("PTY wait task failed: {error}")))?,
        }
    }
}

pub(super) enum ProcessStdin {
    Pipes(ChildStdin),
    Pty(Arc<StdMutex<Box<dyn Write + Send>>>),
}

impl ProcessStdin {
    pub(super) async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Pipes(stdin) => {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(bytes).await?;
                stdin.flush().await
            }
            Self::Pty(writer) => {
                let writer = Arc::clone(writer);
                let bytes = bytes.to_vec();
                tokio::task::spawn_blocking(move || {
                    let mut writer = writer
                        .lock()
                        .map_err(|_| io::Error::other("PTY writer lock poisoned"))?;
                    writer.write_all(&bytes)?;
                    writer.flush()
                })
                .await
                .map_err(|error| io::Error::other(format!("PTY write task failed: {error}")))?
            }
        }
    }
}

pub(super) enum ProcessOutput {
    Pipes {
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
    },
    Pty(Box<dyn Read + Send>),
}

pub(super) fn spawn(
    script: &str,
    workspace: &Path,
    shell: &Shell,
    login: bool,
    tty: bool,
    environment: &[(OsString, OsString)],
) -> io::Result<SpawnedProcess> {
    if tty {
        return spawn_pty(script, workspace, shell, login, environment);
    }

    spawn_pipes(script, workspace, shell, login, environment)
}

fn spawn_pipes(
    script: &str,
    workspace: &Path,
    shell: &Shell,
    login: bool,
    environment: &[(OsString, OsString)],
) -> io::Result<SpawnedProcess> {
    let mut command = Command::new(shell.path());
    command
        .args(shell.args(script, login))
        .current_dir(workspace)
        .env_clear()
        .envs(environment.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);

    let mut child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("spawned shell without a process identifier"))?;
    Ok(SpawnedProcess {
        stdin: child.stdin.take().map(ProcessStdin::Pipes),
        output: ProcessOutput::Pipes {
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
        },
        child: ProcessChild::Pipes(child),
        process_group: ProcessGroupGuard::new(pid),
    })
}

fn spawn_pty(
    script: &str,
    workspace: &Path,
    shell: &Shell,
    login: bool,
    environment: &[(OsString, OsString)],
) -> io::Result<SpawnedProcess> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(pty_error)?;
    let mut command = CommandBuilder::new(shell.path());
    for argument in shell.args(script, login) {
        command.arg(argument);
    }
    command.cwd(workspace);
    command.env_clear();
    for (name, value) in environment {
        command.env(name, value);
    }

    let mut child = pair.slave.spawn_command(command).map_err(pty_error)?;
    let pid = child
        .process_id()
        .ok_or_else(|| io::Error::other("spawned PTY command without a process identifier"))?;
    let reader = pair.master.try_clone_reader().map_err(pty_error)?;
    let writer = pair.master.take_writer().map_err(pty_error)?;
    let wait = tokio::task::spawn_blocking(move || {
        child
            .wait()
            .map(|status| i32::try_from(status.exit_code()).unwrap_or(i32::MAX))
    });

    Ok(SpawnedProcess {
        child: ProcessChild::Pty(wait),
        stdin: Some(ProcessStdin::Pty(Arc::new(StdMutex::new(writer)))),
        output: ProcessOutput::Pty(reader),
        process_group: ProcessGroupGuard::new(pid),
    })
}

fn pty_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .or_else(|| status.signal().map(|signal| 128_i32.saturating_add(signal)))
        .unwrap_or(1)
}

pub(super) struct ProcessGroupGuard {
    process_group: Option<Pid>,
}

impl ProcessGroupGuard {
    fn new(pid: u32) -> Self {
        Self {
            process_group: i32::try_from(pid).ok().map(Pid::from_raw),
        }
    }

    fn terminate(&self) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return Err(io::Error::other("process identifier exceeds i32::MAX"));
        };
        match killpg(process_group, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    pub(super) fn disarm(&mut self) {
        self.process_group = None;
    }

    pub(super) fn terminate_and_disarm(&mut self) -> io::Result<()> {
        let result = self.terminate();
        self.disarm();
        result
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

pub(super) fn sanitized_environment() -> (Vec<(OsString, OsString)>, Vec<String>) {
    let mut environment = Vec::new();
    let mut secrets = Vec::new();
    for (name, value) in env::vars_os() {
        if is_sensitive_name(&name) {
            if let Some(value) = value.to_str().filter(|value| value.len() >= 8) {
                secrets.push(value.to_owned());
            }
        } else {
            environment.push((name, value));
        }
    }
    normalize_environment(&mut environment);
    secrets.sort_unstable_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    (environment, secrets)
}

fn normalize_environment(environment: &mut Vec<(OsString, OsString)>) {
    for (name, value) in NORMALIZED_ENVIRONMENT {
        environment.retain(|(candidate, _)| candidate != name);
        environment.push((name.into(), value.into()));
    }
}

fn is_sensitive_name(name: &OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .split('_')
        .any(|part| SENSITIVE_ENV_PARTS.contains(&part))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{NORMALIZED_ENVIRONMENT, normalize_environment};

    #[test]
    fn normalized_environment_overrides_terminal_and_pager_values() {
        let mut environment = vec![
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("TERM"), OsString::from("xterm-256color")),
            (OsString::from("PAGER"), OsString::from("less")),
        ];

        normalize_environment(&mut environment);

        assert!(environment.contains(&(OsString::from("PATH"), OsString::from("/bin"))));
        for (name, value) in NORMALIZED_ENVIRONMENT {
            assert_eq!(
                environment
                    .iter()
                    .filter(|(candidate, _)| candidate == name)
                    .map(|(_, value)| value)
                    .collect::<Vec<_>>(),
                vec![&OsString::from(value)]
            );
        }
    }
}
