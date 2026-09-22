//! The guest owns CUA state and, for CLI sessions, its private desktop.

use nanocodex_computer::{ComputerConfig, ComputerTools};
use std::path::PathBuf;
use tokio::sync::OnceCell;

#[cfg(all(feature = "desktop", target_os = "linux"))]
const DESKTOP_RUNTIME: &str = "/run/nanocodex-hand-desktop";

#[derive(Default)]
pub(super) struct GuestComputer {
    tools: OnceCell<ComputerTools>,
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

    pub(super) async fn catalog(
        &self,
    ) -> Result<Vec<nanocodex_computer::ProviderTool>, nanocodex_tools::contract::ToolError> {
        // A guest without an installed provider still supplies shell and files.
        // A configured provider that fails discovery returns its real error.
        if ComputerConfig::discover().is_none() {
            return Ok(Vec::new());
        }
        Ok(self.tools().await?.catalog().to_vec())
    }

    pub(super) async fn tools(
        &self,
    ) -> Result<&ComputerTools, nanocodex_tools::contract::ToolError> {
        #[cfg(all(feature = "desktop", target_os = "linux"))]
        self.ensure_desktop().await?;
        self.tools
            .get_or_try_init(|| async {
                let config = ComputerConfig::discover()
                    .ok_or("No upstream Sky MCP provider is configured for this guest")?;
                #[cfg(all(feature = "desktop", target_os = "linux"))]
                let config = {
                    // The guest owns this display. Point the upstream provider at
                    // it explicitly instead of inheriting the host's desktop.
                    let mut config = config;
                    let runtime = PathBuf::from(DESKTOP_RUNTIME);
                    let display = std::fs::read_to_string(runtime.join("display"))?;
                    if !display.starts_with(':')
                        || display.len() > 6
                        || !display[1..].bytes().all(|byte| byte.is_ascii_digit())
                    {
                        return Err("Guest desktop published an invalid X display".into());
                    }
                    config.environment.insert("DISPLAY".into(), display.into());
                    config.environment.insert(
                        "XAUTHORITY".into(),
                        runtime.join("Xauthority").into_os_string(),
                    );
                    config
                        .environment
                        .insert("XDG_RUNTIME_DIR".into(), runtime.into_os_string());
                    config
                        .environment
                        .insert("WAYLAND_DISPLAY".into(), "".into());
                    config
                };
                ComputerTools::connect(config).await
            })
            .await
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
