use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    path::Path,
    process::Stdio,
    sync::Arc,
};

#[cfg(not(unix))]
use std::{
    io::{Read, Write},
    sync::Mutex as StdMutex,
};

#[cfg(unix)]
use nix::{
    errno::Errno,
    libc,
    sys::signal::{Signal, kill, killpg},
    unistd::Pid,
};
#[cfg(unix)]
use portable_pty::MasterPty;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
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
    #[cfg(unix)]
    pub(super) pty_master: Option<Box<dyn MasterPty + Send>>,
}

pub(super) enum ProcessChild {
    Pipes {
        child: Child,
        exit_code: Option<i32>,
    },
    Pty {
        wait: Option<JoinHandle<io::Result<i32>>>,
        exit_code: Option<i32>,
    },
}

impl ProcessChild {
    pub(super) async fn wait(&mut self) -> io::Result<i32> {
        match self {
            Self::Pipes {
                child,
                exit_code: cached,
            } => {
                if let Some(exit_code) = *cached {
                    return Ok(exit_code);
                }
                let exit_code = child.wait().await.map(exit_code)?;
                *cached = Some(exit_code);
                Ok(exit_code)
            }
            Self::Pty {
                wait,
                exit_code: cached,
            } => {
                if let Some(exit_code) = *cached {
                    return Ok(exit_code);
                }
                // Await by mutable reference so a yield timeout can cancel
                // this wait without detaching and losing the sole join handle.
                let result = wait
                    .as_mut()
                    .ok_or_else(|| io::Error::other("PTY wait result is unavailable"))?
                    .await;
                let exit_code = result.map_err(|error| {
                    io::Error::other(format!("PTY wait task failed: {error}"))
                })??;
                *wait = None;
                *cached = Some(exit_code);
                Ok(exit_code)
            }
        }
    }
}

pub(super) enum ProcessStdin {
    #[cfg(unix)]
    Pty(PtyIo),
    #[cfg(not(unix))]
    Pty(Arc<StdMutex<Box<dyn Write + Send>>>),
}

impl ProcessStdin {
    pub(super) async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Pty(io) => io.write_all(bytes).await,
            #[cfg(not(unix))]
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
    #[cfg(unix)]
    Pty(PtyIo),
    #[cfg(not(unix))]
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            let parent_pid = unsafe { libc::getpid() };
            // `process_group(0)` alone does not prevent an owned child from
            // outliving an abruptly terminated runtime. Arm PDEATHSIG in the
            // child and recheck the recorded parent PID to close the fork/exec
            // race where the parent disappears before prctl runs.
            unsafe {
                command.pre_exec(move || set_parent_death_signal(parent_pid));
            }
        }
    }

    let mut child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("spawned shell without a process identifier"))?;
    Ok(SpawnedProcess {
        stdin: None,
        output: ProcessOutput::Pipes {
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
        },
        child: ProcessChild::Pipes {
            child,
            exit_code: None,
        },
        process_group: ProcessGroupGuard::new(pid),
        #[cfg(unix)]
        pty_master: None,
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
    #[cfg(unix)]
    let io = PtyIo::new(
        pair.master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("PTY master has no file descriptor"))?,
    )?;
    #[cfg(not(unix))]
    let reader = pair.master.try_clone_reader().map_err(pty_error)?;
    #[cfg(not(unix))]
    let writer = pair.master.take_writer().map_err(pty_error)?;
    let wait = tokio::task::spawn_blocking(move || {
        child
            .wait()
            .map(|status| i32::try_from(status.exit_code()).unwrap_or(i32::MAX))
    });

    Ok(SpawnedProcess {
        child: ProcessChild::Pty {
            wait: Some(wait),
            exit_code: None,
        },
        #[cfg(unix)]
        stdin: Some(ProcessStdin::Pty(io.clone())),
        #[cfg(not(unix))]
        stdin: Some(ProcessStdin::Pty(Arc::new(StdMutex::new(writer)))),
        #[cfg(unix)]
        output: ProcessOutput::Pty(io),
        #[cfg(not(unix))]
        output: ProcessOutput::Pty(reader),
        process_group: ProcessGroupGuard::new(pid),
        #[cfg(unix)]
        pty_master: Some(pair.master),
    })
}

#[cfg(target_os = "linux")]
fn set_parent_death_signal(parent_pid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::getppid() } != parent_pid {
        unsafe {
            libc::raise(libc::SIGTERM);
        }
    }
    Ok(())
}

fn pty_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

/// Abortable Unix PTY I/O backed by a duplicated nonblocking master fd.
///
/// `portable-pty` exposes blocking `Read`/`Write` trait objects. Driving
/// those on Tokio's blocking pool makes runtime shutdown wait forever when a
/// descendant retains the slave. A duplicated fd keeps the portable owner
/// intact while readiness-based operations remain cancellable.
#[cfg(unix)]
#[derive(Clone)]
pub(super) struct PtyIo {
    fd: Arc<tokio::io::unix::AsyncFd<std::fs::File>>,
}

#[cfg(unix)]
impl PtyIo {
    fn new(master_fd: std::os::fd::RawFd) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, BorrowedFd};

        let fd = unsafe { BorrowedFd::borrow_raw(master_fd) }.try_clone_to_owned()?;
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags == -1
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(io::Error::last_os_error());
        }
        // AsyncFd panics without a Tokio I/O driver; surface a normal spawn
        // error instead of taking down the tool runtime.
        let fd = std::panic::catch_unwind(move || {
            tokio::io::unix::AsyncFd::new(std::fs::File::from(fd))
        })
        .map_err(|_| io::Error::other("PTY I/O requires a Tokio runtime with I/O enabled"))??;
        Ok(Self { fd: Arc::new(fd) })
    }

    pub(super) async fn read(&self, bytes: &mut [u8]) -> io::Result<usize> {
        use std::io::Read;

        self.fd
            .async_io(tokio::io::Interest::READABLE, |mut file| file.read(bytes))
            .await
    }

    async fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;

        while !bytes.is_empty() {
            match self
                .fd
                .async_io(tokio::io::Interest::WRITABLE, |mut file| file.write(bytes))
                .await
            {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .or_else(|| status.signal().map(|signal| 128_i32.saturating_add(signal)))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

pub(crate) struct ProcessGroupGuard {
    #[cfg(unix)]
    process_group: Option<Pid>,
    #[cfg(not(unix))]
    process_group: Option<u32>,
}

impl ProcessGroupGuard {
    pub(crate) fn new(pid: u32) -> Self {
        #[cfg(unix)]
        let process_group = i32::try_from(pid).ok().map(Pid::from_raw);
        #[cfg(not(unix))]
        let process_group = Some(pid);
        Self { process_group }
    }

    #[cfg(unix)]
    pub(super) fn interrupt(&self) -> io::Result<()> {
        self.signal(Signal::SIGINT)
    }

    #[cfg(not(unix))]
    pub(super) fn interrupt(&self) -> io::Result<()> {
        self.terminate()
    }

    #[cfg(unix)]
    fn terminate(&self) -> io::Result<()> {
        self.signal(Signal::SIGKILL)
    }

    #[cfg(unix)]
    pub(super) fn terminate_gracefully(&self) -> io::Result<()> {
        self.signal(Signal::SIGTERM)
    }

    #[cfg(not(unix))]
    pub(super) fn terminate_gracefully(&self) -> io::Result<()> {
        self.terminate()
    }

    #[cfg(unix)]
    fn signal(&self, signal: Signal) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return Err(io::Error::other("process identifier exceeds i32::MAX"));
        };
        match killpg(process_group, signal) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            // If a platform rejects a group signal, retry against the exact
            // direct child rather than broadening the target to a guessed
            // ancestor group. Normal operation remains process-group scoped
            // so descendants are terminated too.
            Err(Errno::EPERM) => match kill(process_group, signal) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
            },
            Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    #[cfg(windows)]
    fn terminate(&self) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return Ok(());
        };
        // `taskkill /T` is the Windows analogue of killing a Unix process
        // group: it targets the child and its descendants. Do not treat a
        // failed command as successful cleanup; callers must retain a real
        // failure rather than falsely acknowledge termination.
        let status = std::process::Command::new("taskkill.exe")
            .args(["/PID", &process_group.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("taskkill exited with {status}")))
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn terminate(&self) -> io::Result<()> {
        Ok(())
    }

    pub(super) const fn disarm(&mut self) {
        self.process_group = None;
    }

    pub(crate) fn terminate_and_disarm(&mut self) -> io::Result<()> {
        self.terminate()?;
        self.disarm();
        Ok(())
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

pub(super) fn sanitized_environment(
    overrides: &[(OsString, OsString)],
) -> (Vec<(OsString, OsString)>, Vec<String>) {
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
    for (name, value) in overrides {
        environment.retain(|(candidate, _)| candidate != name);
        environment.push((name.clone(), value.clone()));
        if is_sensitive_name(name)
            && let Some(value) = value.to_str().filter(|value| value.len() >= 8)
        {
            secrets.push(value.to_owned());
        }
    }
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

/// Returns the ambient environment variables the shell tool withholds from tool
/// subprocesses because their names look sensitive.
///
/// Pass the result to
/// [`ToolsBuilder::process_environment`](crate::ToolsBuilder::process_environment)
/// when the embedder's tools legitimately need them. That is the case behind a
/// credential-injecting proxy, where the variable holds a marker the proxy
/// substitutes at the network boundary rather than a secret — a tool that cannot
/// send the marker cannot authenticate at all, so withholding it only breaks the
/// tool. Forwarded UTF-8 values of at least eight bytes still join the existing
/// redaction list, so they stay masked in tool output.
///
/// Selecting by name is deliberate: forwarding the whole ambient environment
/// would also override the shell normalization (`TERM`, `PAGER`, `NO_COLOR`, ...)
/// that keeps tool output machine-readable.
///
/// # Security
///
/// This function selects variables by name and cannot distinguish proxy-safe
/// markers from real secrets. Passing its result to a tool runtime grants every
/// tool subprocess access to every returned value. Only use it when the embedding
/// boundary deliberately permits that access.
#[cfg(feature = "native")]
#[must_use]
pub fn ambient_sensitive_environment() -> Vec<(OsString, OsString)> {
    env::vars_os()
        .filter(|(name, _)| is_sensitive_name(name))
        .collect()
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

    #[cfg(feature = "native")]
    use std::collections::BTreeSet;

    #[cfg(feature = "native")]
    use super::ambient_sensitive_environment;
    use super::{NORMALIZED_ENVIRONMENT, normalize_environment, sanitized_environment};

    #[cfg(feature = "native")]
    #[test]
    fn ambient_sensitive_environment_partitions_the_ambient_environment() {
        let forwarded: BTreeSet<_> = ambient_sensitive_environment()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let (kept, _) = sanitized_environment(&[]);
        let kept: BTreeSet<_> = kept.into_iter().map(|(name, _)| name).collect();

        // The contract an embedder relies on: forwarding this set restores what
        // the child lost and nothing else, so the two sets partition the ambient
        // environment.
        assert!(kept.is_disjoint(&forwarded));
        for name in std::env::vars_os().map(|(name, _)| name) {
            assert!(
                kept.contains(&name) || forwarded.contains(&name),
                "{name:?} is neither kept nor forwarded"
            );
        }
        for name in &forwarded {
            assert!(
                !NORMALIZED_ENVIRONMENT
                    .iter()
                    .any(|(normalized, _)| name == normalized),
                "{name:?} would override the shell normalization"
            );
        }
    }

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

    #[test]
    fn explicit_environment_overrides_are_retained_and_redacted() {
        let value = OsString::from("proxy-secret-value");
        let (environment, secrets) = sanitized_environment(&[
            (OsString::from("TERM"), OsString::from("mpp-terminal")),
            (OsString::from("NANOCODEX_PROXY_TOKEN"), value.clone()),
        ]);

        assert!(environment.contains(&(OsString::from("TERM"), OsString::from("mpp-terminal"))));
        assert!(environment.contains(&(OsString::from("NANOCODEX_PROXY_TOKEN"), value,)));
        assert!(secrets.iter().any(|secret| secret == "proxy-secret-value"));
    }
}
