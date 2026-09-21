//! Start only the installed, OS-owned publisher. Never elevate or spawn a daemon.
use std::future::Future;

#[derive(Clone, Copy)]
pub(super) enum Platform {
    Mac,
    Linux,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    MacStatus,
    MacStart,
    MacLoad,
    LinuxStatus,
    LinuxStart,
}
impl Action {
    pub(super) fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::MacStatus => ("/bin/launchctl", &["print", "system/com.nanocodex.hand"]),
            // No -k: an already running publisher must never be restarted.
            Self::MacStart => (
                "/bin/launchctl",
                &["kickstart", "system/com.nanocodex.hand"],
            ),
            Self::MacLoad => (
                "/bin/launchctl",
                &[
                    "bootstrap",
                    "system",
                    "/Library/LaunchDaemons/com.nanocodex.hand.plist",
                ],
            ),
            Self::LinuxStatus => (
                "/bin/systemctl",
                &[
                    "--no-ask-password",
                    "show",
                    "nanocodex-hand.service",
                    "--property=LoadState",
                    "--property=ActiveState",
                ],
            ),
            Self::LinuxStart => (
                "/bin/systemctl",
                &["--no-ask-password", "start", "nanocodex-hand.service"],
            ),
        }
    }
}
pub(super) struct Reply {
    pub success: bool,
    pub stdout: String,
}
const INSTALL: &str = "The computer Hand OS service is not installed. Install it once with scripts/install-hand-service.py (see docs/architecture/hands.md), then retry. To use the CLI without a local Hand, set NANOCODEX_DISABLE_HAND=1.";

pub(super) async fn ensure_with<F, Fut>(
    platform: Platform,
    mac_installed: bool,
    mut run: F,
) -> Result<(), String>
where
    F: FnMut(Action) -> Fut,
    Fut: Future<Output = Result<Reply, String>>,
{
    let recovery = match platform {
        Platform::Mac => {
            "Ask an administrator to start the installed service with `sudo launchctl bootstrap system /Library/LaunchDaemons/com.nanocodex.hand.plist` if unloaded, or `sudo launchctl kickstart system/com.nanocodex.hand` if loaded."
        }
        Platform::Linux => {
            "Ask an administrator to run `sudo systemctl start nanocodex-hand.service`."
        }
    };
    let failed = |detail: &str| {
        format!(
            "Could not ensure the computer Hand OS service: {detail}. {recovery} Check the service logs and saved login if it cannot stay running. To use the CLI without a local Hand, set NANOCODEX_DISABLE_HAND=1."
        )
    };
    let start = match platform {
        Platform::Mac => {
            let status = run(Action::MacStatus).await.map_err(|e| failed(&e))?;
            if status.success {
                if status
                    .stdout
                    .lines()
                    .any(|line| line.trim() == "state = running")
                {
                    return Ok(());
                }
                Action::MacStart
            } else if mac_installed {
                Action::MacLoad
            } else {
                return Err(INSTALL.into());
            }
        }
        Platform::Linux => {
            let status = run(Action::LinuxStatus).await.map_err(|e| failed(&e))?;
            if status
                .stdout
                .lines()
                .any(|line| line == "LoadState=not-found")
            {
                return Err(INSTALL.into());
            }
            if !status.success || !status.stdout.lines().any(|line| line == "LoadState=loaded") {
                return Err(failed("systemd could not load the installed service"));
            }
            if status
                .stdout
                .lines()
                .any(|line| line == "ActiveState=active")
            {
                return Ok(());
            }
            Action::LinuxStart
        }
    };
    let reply = run(start).await.map_err(|e| failed(&e))?;
    if !reply.success {
        return Err(failed(
            "the service manager rejected the start request (permission may be required)",
        ));
    }
    // The caller waits for its account-scoped IPC observer to confirm readiness.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, future::ready};

    async fn scenario(
        platform: Platform,
        installed: bool,
        replies: Vec<(Action, bool, &str)>,
    ) -> Result<(), String> {
        let mut replies: VecDeque<_> = replies.into();
        let result = ensure_with(platform, installed, |action| {
            let (expected, success, stdout) =
                replies.pop_front().expect("unexpected service command");
            assert_eq!(action, expected);
            ready(Ok(Reply {
                success,
                stdout: stdout.into(),
            }))
        })
        .await;
        assert!(replies.is_empty(), "expected command was not run");
        result
    }
    #[tokio::test]
    async fn running_services_are_never_restarted() {
        scenario(
            Platform::Mac,
            true,
            vec![(Action::MacStatus, true, "\tstate = running\n")],
        )
        .await
        .unwrap();
        scenario(
            Platform::Linux,
            false,
            vec![(
                Action::LinuxStatus,
                true,
                "LoadState=loaded\nActiveState=active\n",
            )],
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn missing_services_report_installation_without_starting() {
        for (platform, action, stdout) in [
            (Platform::Mac, Action::MacStatus, ""),
            (
                Platform::Linux,
                Action::LinuxStatus,
                "LoadState=not-found\nActiveState=inactive\n",
            ),
        ] {
            let error = scenario(platform, false, vec![(action, false, stdout)])
                .await
                .unwrap_err();
            assert!(error.contains("not installed"));
            assert!(error.contains("scripts/install-hand-service.py"));
        }
    }
    #[tokio::test]
    async fn stopped_services_start_through_their_os_owner() {
        scenario(
            Platform::Mac,
            true,
            vec![
                (Action::MacStatus, true, "state = not running"),
                (Action::MacStart, true, ""),
            ],
        )
        .await
        .unwrap();
        scenario(
            Platform::Mac,
            true,
            vec![(Action::MacStatus, false, ""), (Action::MacLoad, true, "")],
        )
        .await
        .unwrap();
        scenario(
            Platform::Linux,
            false,
            vec![
                (
                    Action::LinuxStatus,
                    true,
                    "LoadState=loaded\nActiveState=failed",
                ),
                (Action::LinuxStart, true, ""),
            ],
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn denied_start_is_actionable() {
        let error = scenario(
            Platform::Linux,
            false,
            vec![
                (
                    Action::LinuxStatus,
                    true,
                    "LoadState=loaded\nActiveState=inactive",
                ),
                (Action::LinuxStart, false, ""),
            ],
        )
        .await
        .unwrap_err();
        assert!(error.contains("sudo systemctl start nanocodex-hand.service"));
        assert!(error.contains("NANOCODEX_DISABLE_HAND=1"));
        let error = scenario(
            Platform::Mac,
            true,
            vec![
                (Action::MacStatus, true, "state = exited"),
                (Action::MacStart, false, ""),
            ],
        )
        .await
        .unwrap_err();
        assert!(error.contains("sudo launchctl kickstart"));
    }
    #[tokio::test]
    async fn missing_service_manager_reports_recovery() {
        let error = ensure_with(Platform::Linux, false, |_| {
            ready(Err("cannot execute service manager".into()))
        })
        .await
        .unwrap_err();
        assert!(error.contains("cannot execute service manager"));
        assert!(error.contains("NANOCODEX_DISABLE_HAND=1"));
    }
    #[test]
    fn commands_never_restart_or_elevate_and_linux_never_prompts() {
        for action in [
            Action::MacStatus,
            Action::MacStart,
            Action::MacLoad,
            Action::LinuxStatus,
            Action::LinuxStart,
        ] {
            let (program, args) = action.command();
            assert!(program.starts_with('/'));
            assert!(
                !args
                    .iter()
                    .any(|arg| matches!(*arg, "sudo" | "restart" | "-k"))
            );
            if program.ends_with("systemctl") {
                assert!(args.contains(&"--no-ask-password"));
            }
        }
    }
}
