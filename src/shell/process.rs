use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    os::unix::process::ExitStatusExt,
    path::Path,
    process::{ExitStatus, Stdio},
};

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

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

pub(super) struct SpawnedProcess {
    pub(super) child: Child,
    pub(super) stdin: Option<ChildStdin>,
    pub(super) stdout: Option<ChildStdout>,
    pub(super) stderr: Option<ChildStderr>,
    pub(super) process_group: ProcessGroupGuard,
}

pub(super) fn spawn(
    script: &str,
    workspace: &Path,
    login: bool,
    environment: &[(OsString, OsString)],
) -> io::Result<SpawnedProcess> {
    let mut command = Command::new("/bin/sh");
    command
        .args([if login { "-lc" } else { "-c" }, script])
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
        .ok_or_else(|| io::Error::other("spawned /bin/sh without a process identifier"))?;
    Ok(SpawnedProcess {
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
        process_group: ProcessGroupGuard::new(pid),
    })
}

pub(super) fn exit_code(status: ExitStatus) -> i32 {
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
    secrets.sort_unstable_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    (environment, secrets)
}

fn is_sensitive_name(name: &OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .split('_')
        .any(|part| SENSITIVE_ENV_PARTS.contains(&part))
}
