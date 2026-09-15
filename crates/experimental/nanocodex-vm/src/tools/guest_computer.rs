//! The guest owns CUA state and, for CLI sessions, its private desktop.

use nanocodex_computer::{ComputerConfig, ComputerTools};
use std::{path::PathBuf, sync::OnceLock};

const DESKTOP_RUNTIME: &str = "/run/nanocodex-hand-desktop";

#[derive(Default)]
pub(super) struct GuestComputer {
    tools: OnceLock<ComputerTools>,
    #[cfg(all(feature = "desktop", target_os = "linux"))]
    workspace: PathBuf,
    #[cfg(all(feature = "desktop", target_os = "linux"))]
    desktop: tokio::sync::Mutex<Option<DesktopTask>>,
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
struct DesktopTask(tokio::task::JoinHandle<Result<(), crate::desktop::Error>>);
#[cfg(all(feature = "desktop", target_os = "linux"))]
impl Drop for DesktopTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl GuestComputer {
    pub(super) fn new(_workspace: PathBuf) -> Self {
        Self {
            #[cfg(all(feature = "desktop", target_os = "linux"))]
            workspace: _workspace,
            ..Self::default()
        }
    }

    pub(super) async fn tools(
        &self,
    ) -> Result<&ComputerTools, nanocodex_tools::contract::ToolError> {
        #[cfg(all(feature = "desktop", target_os = "linux"))]
        self.ensure_desktop().await?;
        Ok(self.tools.get_or_init(|| {
            let mut config = ComputerConfig::discover()
                .unwrap_or_else(|| ComputerConfig::new("/usr/local/bin/nanocodex-computer"));
            config.desktop_runtime = Some(DESKTOP_RUNTIME.into());
            ComputerTools::local(config)
        }))
    }

    #[cfg(all(feature = "desktop", target_os = "linux"))]
    async fn ensure_desktop(&self) -> Result<(), nanocodex_tools::contract::ToolError> {
        use std::time::{Duration, Instant};
        let mut owned = self.desktop.lock().await;
        let ready = PathBuf::from(DESKTOP_RUNTIME).join("ready");
        if ready.is_file() {
            return Ok(());
        }
        if owned.is_none() {
            let workspace = self.workspace.clone();
            *owned = Some(DesktopTask(tokio::spawn(async move {
                crate::desktop::serve(workspace, DESKTOP_RUNTIME.into()).await
            })));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if ready.is_file() {
                return Ok(());
            }
            if owned.as_ref().is_some_and(|task| task.0.is_finished()) {
                let mut task = owned.take().expect("finished owned desktop");
                let result = (&mut task.0).await?;
                result?;
                return Err("Guest desktop exited before becoming ready".into());
            }
            if Instant::now() >= deadline {
                // Dropping the task cancels serve and releases its owned X11 processes.
                owned.take();
                return Err("Guest desktop did not become ready within 30 seconds".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub(super) async fn shutdown(&self) {
        #[cfg(all(feature = "desktop", target_os = "linux"))]
        if let Some(mut task) = self.desktop.lock().await.take() {
            let stopped = tokio::task::spawn_blocking(|| {
                crate::desktop::request(
                    std::path::Path::new(DESKTOP_RUNTIME),
                    serde_json::json!({"action":"shutdown"}),
                )
            })
            .await;
            if !matches!(stopped, Ok(Ok(ref value)) if value["status"] == "ok") {
                task.0.abort();
            }
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), &mut task.0).await;
        }
    }
}
