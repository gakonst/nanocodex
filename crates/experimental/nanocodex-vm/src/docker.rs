//! Docker workspaces using the same retained guest protocol as libkrun.
//!
//! Containers share the Docker host's kernel. This backend does not silently
//! substitute for VM isolation and never mounts host paths or the Docker socket.

use std::{
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nanocodex_tools::ToolsBuilder;
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::tools::{VmToolSession, VmToolSessionError, VmToolSessionHandle, VmTools};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const OWNER_LABEL: &str = "org.nanocodex.hand.lease";

/// Failure to configure, launch, or stop a Docker workspace.
#[derive(Debug, thiserror::Error)]
pub enum DockerWorkspaceError {
    /// An option cannot be represented safely by the Docker launch contract.
    #[error("invalid Docker workspace configuration: {0}")]
    Configuration(String),
    /// The Docker CLI or daemon failed.
    #[error("Docker {operation} failed: {detail}")]
    Docker {
        /// Operation which failed.
        operation: String,
        /// Docker diagnostic or timeout.
        detail: String,
    },
    /// The guest protocol failed.
    #[error(transparent)]
    Session(#[from] VmToolSessionError),
}

/// Builder for an unprivileged container and a persistent named workspace volume.
///
/// The image must contain the Linux guest runtime at
/// `/usr/local/bin/nanocodex-vm-guest`, `/bin/sh`, and `/bin/sync`. The workspace
/// directory in the image must be writable by UID/GID 1000. Images are pulled
/// explicitly by the operator, outside the bounded launch path.
///
/// Network access is disabled by default. [`Self::internet`] explicitly permits
/// ordinary Docker bridge networking; it is not a broker-enforced egress mode.
#[derive(Clone, Debug)]
pub struct DockerWorkspaceBuilder {
    image: String,
    volume: String,
    workspace: String,
    shell: String,
    cpus: u8,
    memory_mib: u32,
    internet: bool,
    runtime: Option<String>,
}

/// A retained container whose standard tools and desktop share one guest session.
///
/// The named workspace volume survives shutdown and container replacement.
/// One deterministic container name acts as a daemon-side lease on the volume;
/// concurrent launches using the same volume fail instead of sharing it.
/// Docker administrators can still access the volume or start other containers.
pub struct DockerWorkspace {
    session: VmToolSession,
    container: Arc<ContainerLease>,
    shutdown: tokio::sync::Mutex<()>,
    workspace: String,
    shell: String,
}

impl DockerWorkspace {
    /// Configures one workspace using an explicitly selected image and volume.
    #[must_use]
    pub fn builder(image: impl Into<String>, volume: impl Into<String>) -> DockerWorkspaceBuilder {
        DockerWorkspaceBuilder {
            image: image.into(),
            volume: volume.into(),
            workspace: "/app".into(),
            shell: "sh".into(),
            cpus: 2,
            memory_mib: 1024,
            internet: false,
            runtime: None,
        }
    }

    /// Absolute workspace path inside the container.
    #[must_use]
    pub fn guest_workspace(&self) -> &str {
        &self.workspace
    }

    /// Clone-cheap control capability for host-owned guest services.
    #[must_use]
    pub fn control(&self) -> VmToolSessionHandle {
        self.session.handle()
    }

    /// Clone-cheap standard tools backed by the retained Docker guest.
    #[must_use]
    pub fn tools(&self) -> VmTools {
        self.session.tools()
    }

    /// Standard workspace tools (including patches and images) for an embedded agent.
    /// Web search, image generation, and planning keep their normal host behavior.
    #[must_use]
    pub fn tools_builder(&self) -> ToolsBuilder {
        self.configure_tools(self.tools().tools_builder())
    }

    /// Process tools for the managed Hand cwd namespace, matching VM attachments.
    #[must_use]
    pub fn attachment_tools_builder(&self) -> ToolsBuilder {
        self.configure_tools(self.tools().attachment_tools_builder())
    }

    fn configure_tools(&self, builder: ToolsBuilder) -> ToolsBuilder {
        builder
            .working_directory(self.workspace.clone())
            .default_shell(self.shell.clone())
    }

    /// Syncs and stops the guest, then removes its container, retaining the volume.
    ///
    /// # Errors
    /// Rejects live tool/control capabilities, or reports protocol/cleanup failures.
    pub async fn shutdown(&self) -> Result<(), DockerWorkspaceError> {
        // A concurrent caller must not force-remove the container while another
        // caller is still syncing the guest filesystem.
        let _shutdown = self.shutdown.lock().await;
        let result = self.session.shutdown().await;
        if matches!(
            result,
            Err(VmToolSessionError::ActiveCapabilities(_) | VmToolSessionError::ActiveRequests(_))
        ) {
            return result.map_err(Into::into);
        }
        self.container.remove().await?;
        result.map_err(Into::into)
    }
}

impl DockerWorkspaceBuilder {
    /// Sets the absolute guest directory mounted from the named volume.
    #[must_use]
    pub fn guest_workspace(mut self, path: impl Into<String>) -> Self {
        self.workspace = path.into();
        self
    }
    /// Sets the shell described by the standard tools.
    #[must_use]
    pub fn shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }
    /// Sets a positive CPU quota.
    #[must_use]
    pub const fn cpus(mut self, cpus: u8) -> Self {
        self.cpus = cpus;
        self
    }
    /// Sets a positive memory limit, with no additional swap allowance.
    #[must_use]
    pub const fn memory_mib(mut self, memory: u32) -> Self {
        self.memory_mib = memory;
        self
    }
    /// Enables ordinary bridge networking, without egress filtering.
    #[must_use]
    pub const fn internet(mut self) -> Self {
        self.internet = true;
        self
    }
    /// Selects a daemon-configured OCI runtime (for example `runsc`).
    /// There is no fallback if the requested runtime is unavailable.
    #[must_use]
    pub fn runtime(mut self, runtime: impl Into<String>) -> Self {
        self.runtime = Some(runtime.into());
        self
    }

    fn validate(&self) -> Result<(), DockerWorkspaceError> {
        let invalid = |message: &str| DockerWorkspaceError::Configuration(message.into());
        if self.image.is_empty()
            || self.image.starts_with('-')
            || self.image.chars().any(char::is_whitespace)
        {
            return Err(invalid("image must be a nonempty Docker image reference"));
        }
        if self.volume.len() < 2
            || !self
                .volume
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            || !self
                .volume
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(invalid(
                "volume must be a named Docker volume, not a host path",
            ));
        }
        // Mounts over runtime/system paths can replace the trusted entrypoint or
        // make it impossible to provide the private temporary filesystem.
        if !Path::new(&self.workspace).is_absolute()
            || self.workspace == "/"
            || self.workspace.contains("//")
            || self.workspace.ends_with('/')
            || self.workspace.split('/').any(|p| p == "." || p == "..")
            || self
                .workspace
                .chars()
                .any(|c| c.is_control() || c == ',' || c == '"')
            || [
                "/bin", "/sbin", "/usr", "/etc", "/dev", "/proc", "/sys", "/run", "/tmp",
            ]
            .iter()
            .any(|p| self.workspace == *p || self.workspace.starts_with(&format!("{p}/")))
        {
            return Err(invalid(
                "workspace must be an absolute data directory outside system and temporary paths",
            ));
        }
        if self.cpus == 0 || self.memory_mib == 0 {
            return Err(invalid("CPU and memory limits must be positive"));
        }
        if self.runtime.as_ref().is_some_and(|r| {
            r.is_empty()
                || r.starts_with('-')
                || !r
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        }) {
            return Err(invalid("runtime must be a Docker runtime name"));
        }
        Ok(())
    }

    fn create_command(&self, lease: &ContainerLease) -> Command {
        let mut command = docker();
        command.args([
            "container",
            "create",
            "--pull=never",
            "--interactive",
            "--init",
            "--log-driver=none",
            "--no-healthcheck",
            "--name",
            &lease.name,
            "--label",
            &format!("{OWNER_LABEL}={}", lease.owner),
            "--label",
            "org.nanocodex.hand.backend=docker",
            "--user",
            "1000:1000",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--pids-limit=512",
            "--cpus",
            &self.cpus.to_string(),
            "--memory",
            &format!("{}m", self.memory_mib),
            "--memory-swap",
            &format!("{}m", self.memory_mib),
            "--network",
            if self.internet { "bridge" } else { "none" },
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,size=256m,mode=1777",
            "--tmpfs",
            "/run:rw,nosuid,nodev,size=64m,mode=1777",
            "--mount",
            &format!(
                "type=volume,source={},target={}",
                self.volume, self.workspace
            ),
            "--workdir",
            &self.workspace,
            "--env",
            &format!("HOME={}/.home", self.workspace),
            "--entrypoint",
            "/bin/sh",
        ]);
        // Docker CLI config can automatically inject proxy URLs (including
        // credentials). Explicit empty values suppress that host-side default.
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "FTP_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "ftp_proxy",
            "all_proxy",
            "no_proxy",
        ] {
            command.args(["--env", &format!("{name}=")]);
        }
        if let Some(runtime) = &self.runtime {
            command.args(["--runtime", runtime]);
        }
        // Positional shell arguments keep workspace paths out of shell source.
        command.args([
            &self.image,
            "-ec",
            "mkdir -p -- \"$HOME\"; exec /usr/local/bin/nanocodex-vm-guest \"$1\"",
            "nanocodex",
            &self.workspace,
        ]);
        command
    }

    /// Checks options, the Docker daemon, the selected runtime, and the local image.
    /// Does not create containers or volumes.
    ///
    /// # Errors
    /// Reports unavailable Docker, non-Linux daemons/images, missing runtimes/images,
    /// and images built for a different daemon architecture.
    pub async fn preflight(&self) -> Result<(), DockerWorkspaceError> {
        self.validate()?;
        let info = run(docker().args(["info", "--format", "{{json .}}"]), "info")
            .await.map_err(|error| DockerWorkspaceError::Configuration(format!(
                "Docker backend is unavailable: {error}. Install the Docker CLI and start a Linux Docker daemon; verify access with `docker info`"
            )))?;
        let info: serde_json::Value = serde_json::from_str(&info).map_err(|error| {
            DockerWorkspaceError::Configuration(format!(
                "invalid Docker daemon information: {error}"
            ))
        })?;
        validate_daemon(&info, self.runtime.as_deref())?;
        let image = run(docker().args(["image", "inspect", "--format", "{{.Os}}/{{.Architecture}}", &self.image]), "image inspect")
            .await.map_err(|error| DockerWorkspaceError::Configuration(format!(
                "Docker image {:?} is unavailable locally: {error}. Build the bundled image with `pnpm build:hand-docker`, or explicitly pull your Hand image before starting; startup never pulls images",
                self.image
            )))?;
        let architecture = match info["Architecture"].as_str().unwrap_or_default() {
            "x86_64" | "amd64" => "amd64",
            "aarch64" | "arm64" => "arm64",
            other => other,
        };
        if image.trim() != format!("linux/{architecture}") {
            return Err(DockerWorkspaceError::Configuration(format!(
                "Docker image {:?} is {}, but this daemon requires linux/{architecture}; rebuild the image for the selected Docker daemon",
                self.image,
                image.trim()
            )));
        }
        Ok(())
    }

    /// Creates a container, attaches its stdio, and waits for typed guest readiness.
    ///
    /// # Errors
    /// Fails on invalid options, unavailable Docker/image/runtime, an occupied
    /// workspace lease, or a guest which does not become ready within 30 seconds.
    /// No KVM device is needed. Startup never changes the selected backend.
    pub async fn launch(self) -> Result<DockerWorkspace, DockerWorkspaceError> {
        self.preflight().await?;
        let container = Arc::new(ContainerLease {
            name: format!(
                "nanocodex-hand-{}",
                hex::encode(Sha256::digest(self.volume.as_bytes()))
            ),
            owner: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            removed: AtomicBool::new(false),
        });
        // Finish creation even if the caller cancels launch. Its lease stays
        // alive until Docker has answered, then cleans up if nobody received it.
        let creating = Arc::clone(&container);
        let mut create_command = self.create_command(&container);
        tokio::spawn(async move {
            let result = run(
                &mut create_command,
                "container create (workspace may already be in use)",
            )
            .await;
            if result.is_err() {
                // A duplicate launch's label does not match the current owner.
                let _ = creating.remove().await;
            }
            result
        })
        .await
        .map_err(|error| DockerWorkspaceError::Docker {
            operation: "container create".into(),
            detail: error.to_string(),
        })??;
        let mut command = docker();
        command.args([
            "container",
            "start",
            "--attach",
            "--interactive",
            &container.name,
        ]);
        let session = match VmToolSession::spawn(&mut command) {
            Ok(session) => session,
            Err(error) => {
                container.remove().await?;
                return Err(error.into());
            }
        };
        session.retain(Arc::clone(&container));
        let ready = tokio::time::timeout(COMMAND_TIMEOUT, session.ready())
            .await
            .unwrap_or(Err(VmToolSessionError::StartupTimeout(COMMAND_TIMEOUT)));
        if let Err(error) = ready {
            session.terminate().await;
            container.remove().await?;
            return Err(error.into());
        }
        Ok(DockerWorkspace {
            session,
            container,
            shutdown: tokio::sync::Mutex::new(()),
            workspace: self.workspace,
            shell: self.shell,
        })
    }
}

fn validate_daemon(
    info: &serde_json::Value,
    runtime: Option<&str>,
) -> Result<(), DockerWorkspaceError> {
    if info["OSType"] != "linux" {
        return Err(DockerWorkspaceError::Configuration(
            "Docker Hands require a Linux Docker daemon; switch Docker to Linux containers or select a Linux Docker context".into()
        ));
    }
    if let Some(runtime) = runtime
        && !info["Runtimes"]
            .as_object()
            .is_some_and(|runtimes| runtimes.contains_key(runtime))
    {
        return Err(DockerWorkspaceError::Configuration(format!(
            "Docker runtime {runtime:?} is not configured on this daemon; install/configure it or omit --runtime to use the daemon default. No fallback was attempted"
        )));
    }
    Ok(())
}

fn docker() -> Command {
    // Docker's connection configuration stays on the host. No host environment
    // variable is forwarded into the container unless explicitly set above.
    let mut command = Command::new("docker");
    command.stdin(Stdio::null()).kill_on_drop(true);
    command
}

async fn run(command: &mut Command, operation: &str) -> Result<String, DockerWorkspaceError> {
    let fail = |detail: String| DockerWorkspaceError::Docker {
        operation: operation.into(),
        detail,
    };
    let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| fail(format!("timed out after {COMMAND_TIMEOUT:?}")))?
        .map_err(|error| fail(error.to_string()))?;
    if !output.status.success() {
        return Err(fail(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

struct ContainerLease {
    name: String,
    owner: String,
    removed: AtomicBool,
}

impl ContainerLease {
    async fn remove(&self) -> Result<(), DockerWorkspaceError> {
        if self.removed.load(Ordering::Acquire) {
            return Ok(());
        }
        // Match both the deterministic name and this launch's unique label,
        // then remove by immutable ID. A duplicate launch or a replacement
        // container can never be removed by this lease. No match is idempotent.
        let ids = run(
            docker().args([
                "container",
                "ls",
                "--all",
                "--no-trunc",
                "--filter",
                &format!("name=^/{}$", self.name),
                "--filter",
                &format!("label={OWNER_LABEL}={}", self.owner),
                "--format",
                "{{.ID}}",
            ]),
            "container lookup",
        )
        .await?;
        for id in ids.lines().filter(|id| !id.is_empty()) {
            run(
                docker().args(["container", "rm", "--force", "--volumes", id]),
                "container rm",
            )
            .await?;
        }
        self.removed.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for ContainerLease {
    fn drop(&mut self) {
        if self.removed.load(Ordering::Acquire) {
            return;
        }
        let lease = Self {
            name: self.name.clone(),
            owner: self.owner.clone(),
            removed: AtomicBool::new(true),
        };
        // A runtime may itself be shutting down. A bounded independent reaper
        // also works when the final capability is dropped outside Tokio.
        let _ = std::thread::Builder::new()
            .name("docker-hand-cleanup".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    lease.removed.store(false, Ordering::Release);
                    let _ = runtime.block_on(lease.remove());
                }
                // Failed cleanup must not recursively spawn another reaper.
                lease.removed.store(true, Ordering::Release);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_mounts_and_resource_limits_fail_before_docker() {
        for volume in ["/tmp/work", "../work", "x,y", "-work", "", "x"] {
            assert!(
                DockerWorkspace::builder("image", volume)
                    .validate()
                    .is_err(),
                "{volume}"
            );
        }
        for workspace in [
            "/",
            "relative",
            "/app/../etc",
            "/usr/local",
            "/run",
            "/app,readonly",
            "/app\n",
            "//usr",
            "/app/",
        ] {
            assert!(
                DockerWorkspace::builder("image", "work")
                    .guest_workspace(workspace)
                    .validate()
                    .is_err(),
                "{workspace}"
            );
        }
        assert!(
            DockerWorkspace::builder("image", "work")
                .cpus(0)
                .validate()
                .is_err()
        );
        assert!(
            DockerWorkspace::builder("image", "work")
                .memory_mib(0)
                .validate()
                .is_err()
        );
        assert!(
            DockerWorkspace::builder("image", "work")
                .runtime("--privileged")
                .validate()
                .is_err()
        );
    }

    #[test]
    fn launch_contract_does_not_inherit_host_privileges_or_network() {
        let lease = ContainerLease {
            name: "hand".into(),
            owner: "owner".into(),
            removed: AtomicBool::new(true),
        };
        let command = DockerWorkspace::builder("image", "workspace").create_command(&lease);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect();
        for pair in [
            ["--user", "1000:1000"],
            ["--network", "none"],
            ["--mount", "type=volume,source=workspace,target=/app"],
        ] {
            assert!(args.windows(2).any(|a| a == pair));
        }
        for variable in [
            "HTTP_PROXY=",
            "HTTPS_PROXY=",
            "ALL_PROXY=",
            "http_proxy=",
            "https_proxy=",
            "all_proxy=",
        ] {
            assert!(args.windows(2).any(|pair| pair == ["--env", variable]));
        }
        assert!(args.contains(&"--log-driver=none"));
        assert!(args.contains(&"--read-only"));
        assert!(args.contains(&"--cap-drop=ALL"));
        assert!(args.contains(&"--security-opt=no-new-privileges"));
        assert!(!args.contains(&"--privileged"));
        assert!(!args.contains(&"--pid=host"));
        assert!(!args.contains(&"--rm"));
    }
}
