#![allow(
    unsafe_code,
    reason = "this module is the audited post-fork Linux parent-death boundary"
)]

use std::process::Command;

#[cfg(target_os = "linux")]
use std::{io, os::unix::process::CommandExt as _};

/// Configures a directly owned child to die if its spawning process dies.
///
/// Linux uses `PR_SET_PDEATHSIG` and closes the fork-to-registration race.
/// Other supported hosts retain normal owned-child drop cleanup.
#[cfg(target_os = "linux")]
pub fn terminate_child_with_parent(command: &mut Command) {
    use nix::{sys::signal::Signal, unistd};

    let expected_parent = unistd::getpid();
    // SAFETY: the closure invokes only async-signal-safe syscalls between
    // fork and exec. Comparing the parent closes the race where the owner
    // dies immediately before PR_SET_PDEATHSIG is installed.
    unsafe {
        command.pre_exec(move || {
            nix::sys::prctl::set_pdeathsig(Signal::SIGKILL).map_err(io::Error::from)?;
            if unistd::getppid() != expected_parent {
                return Err(io::Error::from_raw_os_error(
                    nix::errno::Errno::ESRCH as i32,
                ));
            }
            Ok(())
        });
    }
}

/// Keeps the same owned-child API on hosts without Linux parent-death signals.
#[cfg(not(target_os = "linux"))]
pub const fn terminate_child_with_parent(_command: &mut Command) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    use super::terminate_child_with_parent;

    #[test]
    fn configured_child_is_killed_when_its_owner_is_sigkilled() {
        const CHILD_DIRECTORY: &str = "NANOCODEX_PDEATHSIG_TEST_CHILD";
        if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
            let directory = PathBuf::from(directory);
            let mut command = Command::new("/bin/sleep");
            command.arg("60");
            terminate_child_with_parent(&mut command);
            let mut child = command.spawn().unwrap();
            fs::write(directory.join("owned-pid"), child.id().to_string()).unwrap();
            child.wait().unwrap();
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "child_lifetime::tests::configured_child_is_killed_when_its_owner_is_sigkilled",
                "--nocapture",
            ])
            .env(CHILD_DIRECTORY, directory.path())
            .spawn()
            .unwrap();
        let record = directory.path().join("owned-pid");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !record.is_file() {
            assert!(Instant::now() < deadline, "owner never spawned its child");
            assert!(owner.try_wait().unwrap().is_none(), "owner exited early");
            thread::sleep(Duration::from_millis(10));
        }
        let owned_pid = fs::read_to_string(record).unwrap();
        owner.kill().unwrap();
        owner.wait().unwrap();
        let process = PathBuf::from("/proc").join(owned_pid.trim());
        let deadline = Instant::now() + Duration::from_secs(5);
        while process.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!process.exists(), "owned child survived its worker process");
    }
}
