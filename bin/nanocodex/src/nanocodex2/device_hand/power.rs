//! Idle system sleep assertion owned by the standalone publisher, not a socket.
#[cfg(any(target_os = "macos", test))]
use std::{
    ffi::OsStr,
    io,
    process::{Child, Command, Stdio},
};

pub(super) struct KeepAwake {
    #[cfg(any(target_os = "macos", test))]
    child: Child,
}

impl KeepAwake {
    pub(super) fn acquire() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            if !enabled(
                true,
                std::env::var_os("NANOCODEX_HAND_KEEP_AWAKE").as_deref(),
            ) {
                return None;
            }
            match Self::spawn(command(std::process::id())) {
                Ok(guard) => Some(guard),
                Err(error) => {
                    tracing::warn!(%error, "cannot start Hand idle sleep inhibition; continuing without it");
                    None
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        None
    }

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
fn enabled(macos: bool, value: Option<&OsStr>) -> bool {
    macos && value != Some(OsStr::new("0"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_only_default_and_explicit_opt_out() {
        for value in [None, Some(OsStr::new("1")), Some(OsStr::new(""))] {
            assert!(enabled(true, value));
            assert!(!enabled(false, value));
        }
        assert!(!enabled(true, Some(OsStr::new("0"))));
        assert!(!enabled(false, Some(OsStr::new("0"))));
    }

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
