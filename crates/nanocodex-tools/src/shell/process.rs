use std::{
    env,
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
};

#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::process::{ChildStderr, ChildStdout, Command};
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
    pub(super) process_group: SharedProcessGroup,
}

// The session owns this handle and its termination guard; the task owns the
// OS child so it keeps reaping even while nobody polls the session.
pub(super) struct ProcessChild {
    wait: Option<JoinHandle<io::Result<i32>>>,
    completed: Option<io::Result<i32>>,
    process_group: SharedProcessGroup,
}

impl ProcessChild {
    fn new(
        mut try_wait: impl FnMut() -> io::Result<Option<i32>> + Send + 'static,
        process_group: SharedProcessGroup,
    ) -> Self {
        let group = process_group.clone();
        let wait = tokio::spawn(async move {
            loop {
                // Serialize reaping and guard retirement with signals/drop.
                // Never leave an exited child's guard armed until a later poll.
                {
                    let mut guard = group.0.lock().unwrap_or_else(|e| e.into_inner());
                    // Observe exit without releasing the PID, clean up its
                    // group, then reap. A mutex alone cannot prevent the OS
                    // from recycling a PID between try_wait and killpg.
                    let ready = guard.prepare_to_reap();
                    match ready.and_then(|ready| if ready { try_wait() } else { Ok(None) }) {
                        Ok(None) => {}
                        Ok(Some(status)) => {
                            guard.disarm();
                            return Ok(status);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            // ECHILD can mean another waiter already reaped
                            // this PID. Do not signal an unverified identity.
                            guard.disarm();
                            return Err(error);
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        Self {
            wait: Some(wait),
            completed: None,
            process_group,
        }
    }

    pub(super) async fn wait(&mut self) -> io::Result<i32> {
        if self.completed.is_none() {
            // Borrow the handle: cancelling a tool poll must not lose the result.
            let result = self
                .wait
                .as_mut()
                .ok_or_else(|| io::Error::other("shell wait result is unavailable"))?
                .await
                .map_err(|error| io::Error::other(format!("shell wait task failed: {error}")))
                .and_then(|result| result);
            self.wait = None;
            self.completed = Some(result);
        }
        match self.completed.as_ref().expect("completed wait") {
            Ok(status) => Ok(*status),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    }
}

impl Drop for ProcessChild {
    fn drop(&mut self) {
        // Dropping a JoinHandle detaches it. Signal first, then let the waiter
        // reap the child rather than aborting the task and abandoning ownership.
        let _ = self.process_group.terminate_and_disarm();
    }
}

#[derive(Clone)]
pub(super) struct SharedProcessGroup(Arc<StdMutex<ProcessGroupGuard>>);

impl SharedProcessGroup {
    fn new(pid: u32) -> Self {
        Self(Arc::new(StdMutex::new(ProcessGroupGuard::new(pid))))
    }

    pub(super) fn interrupt(&self) -> io::Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).interrupt()
    }

    pub(super) fn terminate_and_disarm(&self) -> io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .terminate_and_disarm()
    }
}

pub(super) enum ProcessStdin {
    Pty(Arc<StdMutex<Box<dyn Write + Send>>>),
}

impl ProcessStdin {
    pub(super) async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("spawned shell without a process identifier"))?;
    let process_group = SharedProcessGroup::new(pid);
    Ok(SpawnedProcess {
        stdin: None,
        output: ProcessOutput::Pipes {
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
        },
        child: ProcessChild::new(
            move || child.try_wait().map(|status| status.map(exit_code)),
            process_group.clone(),
        ),
        process_group,
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

    // Allocate fallible I/O handles before spawning so an allocation error
    // cannot leave a child running without its termination guard and waiter.
    let reader = pair.master.try_clone_reader().map_err(pty_error)?;
    let writer = pair.master.take_writer().map_err(pty_error)?;
    let mut child = pair.slave.spawn_command(command).map_err(pty_error)?;
    let pid = child
        .process_id()
        .ok_or_else(|| io::Error::other("spawned PTY command without a process identifier"))?;
    let process_group = SharedProcessGroup::new(pid);
    let child = ProcessChild::new(
        move || {
            child.try_wait().map(|status| {
                status.map(|status| i32::try_from(status.exit_code()).unwrap_or(i32::MAX))
            })
        },
        process_group.clone(),
    );

    Ok(SpawnedProcess {
        child,
        stdin: Some(ProcessStdin::Pty(Arc::new(StdMutex::new(writer)))),
        output: ProcessOutput::Pty(reader),
        process_group,
    })
}

fn pty_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
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

    // These Unix targets expose waitid through rustix. WNOWAIT keeps the
    // exited leader waitable, reserving its PID until group cleanup finishes.
    #[cfg(all(
        unix,
        not(any(
            target_os = "openbsd",
            target_os = "redox",
            target_os = "horizon",
            target_os = "cygwin"
        ))
    ))]
    fn prepare_to_reap(&mut self) -> io::Result<bool> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};

        let Some(group) = self.process_group else {
            return Ok(true);
        };
        let pid = rustix::process::Pid::from_raw(group.as_raw())
            .ok_or_else(|| io::Error::other("invalid shell process identifier"))?;
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(None) => Ok(false),
            Ok(Some(_)) => {
                let _ = self.terminate_and_disarm();
                Ok(true)
            }
            Err(error) => Err(error.into()),
        }
    }

    // Without a non-reaping exit observation, retire the guard on completion
    // without sending a post-reap signal to a potentially recycled identifier.
    #[cfg(not(all(
        unix,
        not(any(
            target_os = "openbsd",
            target_os = "redox",
            target_os = "horizon",
            target_os = "cygwin"
        ))
    )))]
    fn prepare_to_reap(&mut self) -> io::Result<bool> {
        Ok(true)
    }

    #[cfg(unix)]
    pub(super) fn interrupt(&self) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return Ok(());
        };
        match killpg(process_group, Signal::SIGINT) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    #[cfg(not(unix))]
    pub(super) fn interrupt(&self) -> io::Result<()> {
        self.terminate()
    }

    #[cfg(unix)]
    fn terminate(&self) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return Ok(());
        };
        match killpg(process_group, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    #[cfg(windows)]
    fn terminate(&self) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return Ok(());
        };
        // `taskkill /T` is the Windows analogue of killing a Unix process group: it terminates
        // the child and processes descended from it. A non-zero exit commonly means the child
        // exited between the wait and cleanup paths, which is equivalent to ESRCH on Unix.
        std::process::Command::new("taskkill.exe")
            .args(["/PID", &process_group.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|_| ())
    }

    #[cfg(not(any(unix, windows)))]
    fn terminate(&self) -> io::Result<()> {
        Ok(())
    }

    pub(super) const fn disarm(&mut self) {
        self.process_group = None;
    }

    pub(crate) fn terminate_and_disarm(&mut self) -> io::Result<()> {
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

    #[cfg(all(
        unix,
        not(any(
            target_os = "openbsd",
            target_os = "redox",
            target_os = "horizon",
            target_os = "cygwin"
        ))
    ))]
    #[tokio::test]
    async fn group_is_retired_before_the_leader_is_reaped() {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        use std::os::unix::process::CommandExt;

        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .process_group(0)
            .spawn()
            .expect("spawn group leader");
        let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        let mut guard = super::ProcessGroupGuard::new(child.id());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !guard.prepare_to_reap().expect("observe exit") {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("leader exits");
        assert!(
            guard.process_group.is_none(),
            "group must already be retired"
        );
        assert!(
            waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
            )
            .expect("leader identity is still retained")
            .is_some()
        );
        assert_eq!(child.wait().expect("reap leader").code(), Some(23));
        // Later session cleanup must be an inert operation after reaping.
        guard.interrupt().unwrap();
        guard.terminate_and_disarm().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn interrupted_reap_retries_and_caches_the_exit_status() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .process_group(0)
            .spawn()
            .expect("spawn group leader");
        let group = super::SharedProcessGroup::new(child.id());
        let mut interrupted = false;
        let mut child = super::ProcessChild::new(
            move || {
                if !interrupted {
                    interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                child.try_wait().map(|status| status.map(super::exit_code))
            },
            group,
        );
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                .await
                .unwrap()
                .unwrap(),
            23
        );
        assert_eq!(child.wait().await.unwrap(), 23);
    }

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
