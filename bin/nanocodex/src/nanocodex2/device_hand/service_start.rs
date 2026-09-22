//! Start only the installed, OS-owned publisher. Never elevate or spawn a daemon.
use std::future::Future;

#[derive(Clone, Copy)]
pub(super) enum Platform {
    Mac,
    Linux,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    MacGuiStatus,
    MacGuiStart,
    MacGuiLoad,
    MacStatus,
    MacStart,
    MacLoad,
    LinuxStatus,
    LinuxStart,
}
impl Action {
    pub(super) fn command(
        self,
        gui: Option<(u32, &std::path::Path)>,
    ) -> (&'static str, Vec<String>) {
        if matches!(
            self,
            Self::MacGuiStatus | Self::MacGuiStart | Self::MacGuiLoad
        ) {
            let (uid, plist) = gui.expect("GUI actions require current-user context");
            let domain = format!("gui/{uid}");
            let service = format!("{domain}/com.nanocodex.hand");
            return (
                "/bin/launchctl",
                match self {
                    Self::MacGuiStatus => vec!["print".into(), service],
                    Self::MacGuiStart => vec!["kickstart".into(), service],
                    _ => vec![
                        "bootstrap".into(),
                        domain,
                        plist.to_string_lossy().into_owned(),
                    ],
                },
            );
        }
        let (program, args): (&str, &[&str]) = match self {
            Self::MacGuiStatus | Self::MacGuiStart | Self::MacGuiLoad => unreachable!(),
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
        };
        (program, args.iter().map(|arg| (*arg).to_owned()).collect())
    }
}
pub(super) struct Reply {
    pub success: bool,
    pub stdout: String,
}
const INSTALL: &str = "The computer Hand OS service is not installed. On macOS, install it once with `nanocodex hand install`; on Linux, use scripts/install-hand-service.py (see docs/architecture/hands.md), then retry. To use the CLI without a local Hand, set NANOCODEX_DISABLE_HAND=1.";

pub(super) async fn ensure_with<F, Fut>(
    platform: Platform,
    mac_installed: bool,
    gui_installed: Option<bool>,
    mut run: F,
) -> Result<(), String>
where
    F: FnMut(Action) -> Fut,
    Fut: Future<Output = Result<Reply, String>>,
{
    let recovery = match platform {
        Platform::Mac => {
            "For a current-user LaunchAgent, use `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.nanocodex.hand.plist` if unloaded, or `launchctl kickstart gui/$(id -u)/com.nanocodex.hand` if loaded. For a system service, ask an administrator to start the installed service with `sudo launchctl bootstrap system /Library/LaunchDaemons/com.nanocodex.hand.plist` if unloaded, or `sudo launchctl kickstart system/com.nanocodex.hand` if loaded."
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
            let gui = if gui_installed.is_some() {
                Some(run(Action::MacGuiStatus).await.map_err(|e| failed(&e))?)
            } else {
                None
            };
            let running = |reply: &Reply| {
                reply.success
                    && reply
                        .stdout
                        .lines()
                        .any(|line| line.trim() == "state = running")
            };
            // Inspect both domains before starting either; a running publisher wins.
            if running(&status) || gui.as_ref().is_some_and(running) {
                return Ok(());
            }
            if gui.as_ref().is_some_and(|reply| reply.success) {
                Action::MacGuiStart
            } else if status.success {
                Action::MacStart
            } else if gui_installed == Some(true) {
                Action::MacGuiLoad
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
        let result = ensure_with(platform, installed, None, |action| {
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
        let error = ensure_with(Platform::Linux, false, None, |_| {
            ready(Err("cannot execute service manager".into()))
        })
        .await
        .unwrap_err();
        assert!(error.contains("cannot execute service manager"));
        assert!(error.contains("NANOCODEX_DISABLE_HAND=1"));
    }
    #[tokio::test]
    async fn gui_selection_checks_both_domains_and_starts_only_one_owner() {
        for (system, gui, installed, expected) in [
            (Some("state = exited"), Some("state = running"), true, None),
            (Some("state = running"), Some("state = exited"), true, None),
            (None, Some("state = running"), false, None),
            (
                Some("state = exited"),
                Some("state = exited"),
                true,
                Some(Action::MacGuiStart),
            ),
            (Some("state = exited"), None, true, Some(Action::MacStart)),
            (None, None, true, Some(Action::MacGuiLoad)),
            (None, None, false, Some(Action::MacLoad)),
        ] {
            let mut replies = VecDeque::from([
                (Action::MacStatus, system.is_some(), system.unwrap_or("")),
                (Action::MacGuiStatus, gui.is_some(), gui.unwrap_or("")),
            ]);
            if let Some(action) = expected {
                replies.push_back((action, true, ""));
            }
            ensure_with(Platform::Mac, true, Some(installed), |action| {
                let (expected, success, stdout) = replies.pop_front().expect("unexpected command");
                assert_eq!(action, expected);
                ready(Ok(Reply {
                    success,
                    stdout: stdout.into(),
                }))
            })
            .await
            .unwrap();
            assert!(replies.is_empty());
        }
    }
    #[tokio::test]
    async fn gui_start_failure_does_not_fall_back_to_a_second_publisher() {
        let mut actions =
            VecDeque::from([Action::MacStatus, Action::MacGuiStatus, Action::MacGuiStart]);
        let error = ensure_with(Platform::Mac, true, Some(true), |action| {
            assert_eq!(actions.pop_front(), Some(action));
            ready(Ok(Reply {
                success: action == Action::MacGuiStatus,
                stdout: "state = exited".into(),
            }))
        })
        .await
        .unwrap_err();
        assert!(actions.is_empty());
        assert!(error.contains("launchctl kickstart gui/"));
    }
    #[tokio::test]
    async fn neither_mac_service_installed_reports_installation() {
        let mut actions = VecDeque::from([Action::MacStatus, Action::MacGuiStatus]);
        let error = ensure_with(Platform::Mac, false, Some(false), |action| {
            assert_eq!(actions.pop_front(), Some(action));
            ready(Ok(Reply {
                success: false,
                stdout: String::new(),
            }))
        })
        .await
        .unwrap_err();
        assert!(actions.is_empty());
        assert!(error.contains("not installed"));
        assert!(error.contains("nanocodex hand install"));
    }
    #[test]
    fn gui_commands_target_the_current_user_and_preserve_spaces() {
        let path =
            std::path::Path::new("/Users/test user/Library/LaunchAgents/com.nanocodex.hand.plist");
        for (action, expected) in [
            (
                Action::MacGuiStatus,
                vec!["print", "gui/502/com.nanocodex.hand"],
            ),
            (
                Action::MacGuiStart,
                vec!["kickstart", "gui/502/com.nanocodex.hand"],
            ),
            (
                Action::MacGuiLoad,
                vec!["bootstrap", "gui/502", path.to_str().unwrap()],
            ),
        ] {
            assert_eq!(
                action.command(Some((502, path))),
                (
                    "/bin/launchctl",
                    expected.into_iter().map(str::to_owned).collect()
                )
            );
        }
    }
    #[test]
    fn commands_never_restart_or_elevate_and_linux_never_prompts() {
        for action in [
            Action::MacGuiStatus,
            Action::MacGuiStart,
            Action::MacGuiLoad,
            Action::MacStatus,
            Action::MacStart,
            Action::MacLoad,
            Action::LinuxStatus,
            Action::LinuxStart,
        ] {
            let (program, args) = action.command(Some((
                502,
                std::path::Path::new("/Users/test/Library/LaunchAgents/com.nanocodex.hand.plist"),
            )));
            assert!(program.starts_with('/'));
            assert!(
                !args
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "sudo" | "restart" | "-k"))
            );
            if program.ends_with("systemctl") {
                assert!(args.iter().any(|arg| arg == "--no-ask-password"));
            }
        }
    }
}
