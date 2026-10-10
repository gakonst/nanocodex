//! Idle system sleep assertion owned by the standalone publisher, not a socket.
#[cfg(any(target_os = "macos", test))]
use std::{
    io,
    process::{Child, Command, Stdio},
};

pub(super) struct KeepAwake {
    #[cfg(any(target_os = "macos", test))]
    child: Child,
}

impl KeepAwake {
    #[cfg(any(target_os = "macos", test))]
    fn spawn(mut command: Command) -> io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(Self {
            child: command.spawn()?,
        })
    }
}

#[cfg(any(target_os = "macos", test))]
fn command(pid: u32) -> Command {
    let mut command = Command::new("/usr/bin/caffeinate");
    // -w also releases the assertion if the daemon dies without running Drop.
    // Only idle system sleep: no display assertion or lid-close override.
    command.args(["-i", "-w", &pid.to_string()]);
    command
}

#[cfg(any(target_os = "macos", test))]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        // Reap the direct child even on early returns or unwinding. caffeinate
        // does not launch a subprocess when used with -w.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A small owned watcher applies preference changes across socket reconnects.
/// Shutdown wakes and joins it before the publisher lock is released, so the
/// old owner's cleanup cannot remove a new owner's receipt or assertion.
#[cfg(target_os = "macos")]
pub(super) struct Monitor {
    stop: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl Monitor {
    pub(super) fn start(home: std::path::PathBuf) -> std::io::Result<Self> {
        let stop = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let stopping = stop.clone();
        let worker = std::thread::Builder::new().name("hand-power".into()).spawn(move || {
            use crate::hand_keep_awake as preference;
            let overridden = std::env::var_os(preference::ENVIRONMENT).as_deref() == Some(std::ffi::OsStr::new("0"));
            let mut assertion: Option<KeepAwake> = None;
            let mut configured = true;
            let mut previous = serde_json::Value::Null;
            loop {
                let mut problem = match preference::configured(&home) {
                    Ok(value) => { configured = value; None }
                    Err(error) => Some(error.to_string()),
                };
                let enabled = configured && !overridden;
                if let Some(guard) = assertion.as_mut() {
                    match guard.child.try_wait() {
                        Ok(None) => {}
                        Ok(Some(status)) => {
                            problem = Some(format!("Hand sleep assertion helper exited: {status}"));
                            assertion = None;
                        }
                        Err(error) => {
                            problem = Some(error.to_string());
                            assertion = None;
                        }
                    }
                }
                if !enabled { assertion = None; }
                else if assertion.is_none() {
                    match KeepAwake::spawn(command(std::process::id())) {
                        Ok(guard) => assertion = Some(guard),
                        Err(error) => problem = Some(error.to_string()),
                    }
                }
                let state = serde_json::json!({"daemon_pid": std::process::id(),
                    "configured": configured, "active": assertion.is_some(),
                    "environment_override": overridden, "error": problem});
                if state != previous {
                    if let Some(error) = problem.as_deref() {
                        tracing::warn!(%error, "Hand keep-awake setting could not be applied completely");
                    }
                    match preference::write(&preference::state_path(&home), &state) {
                        Ok(()) => previous = state,
                        Err(error) => tracing::warn!(%error, "Cannot publish Hand keep-awake status"),
                    }
                }
                let (lock, changed) = &*stopping;
                let stop = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let (stop, _) = changed.wait_timeout_while(stop, std::time::Duration::from_secs(1), |stop| !*stop)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *stop { break; }
            }
            drop(assertion);
            let _ = std::fs::remove_file(preference::state_path(&home));
        })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for Monitor {
    fn drop(&mut self) {
        let (lock, changed) = &*self.stop;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_asserts_only_idle_sleep_and_watches_daemon() {
        let command = command(12345);
        assert_eq!(command.get_program(), "/usr/bin/caffeinate");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-i", "-w", "12345"]
        );
    }

    #[test]
    fn spawn_failure_is_returned_without_a_guard() {
        let directory = tempfile::tempdir().unwrap();
        assert!(KeepAwake::spawn(Command::new(directory.path().join("missing"))).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn guard_keeps_child_alive_until_drop_and_reaps_it() {
        let mut command = Command::new("/bin/sleep");
        command.arg("60");
        let mut guard = KeepAwake::spawn(command).unwrap();
        let pid = guard.child.id();
        assert!(guard.child.try_wait().unwrap().is_none());
        drop(guard);
        // A reaped child is no longer waitable, rather than a zombie left for
        // the long-lived service to accumulate.
        assert_eq!(
            nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid as i32), None),
            Err(nix::errno::Errno::ECHILD)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_assertion_is_visible_and_released_on_drop() {
        let guard = KeepAwake::spawn(command(std::process::id())).unwrap();
        let owner = format!("pid {}(caffeinate):", guard.child.id());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let output = Command::new("/usr/bin/pmset")
                .args(["-g", "assertions"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let assertions = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = assertions.lines().find(|line| line.contains(&owner)) {
                assert!(line.contains("PreventUserIdleSystemSleep"), "{line}");
                assert!(!line.contains("PreventUserIdleDisplaySleep"), "{line}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "caffeinate did not acquire its assertion"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        drop(guard);
        let output = Command::new("/usr/bin/pmset")
            .args(["-g", "assertions"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&owner));
    }
}
