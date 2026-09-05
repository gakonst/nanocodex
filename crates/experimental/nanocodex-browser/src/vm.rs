//! The deterministic Nanocodex browser controller hosted in one libkrun VM.
//!
//! This module composes two otherwise independent components:
//!
//! - this crate owns typed browser actions, diagnostics, artifacts,
//!   authentication policy, and the ordinary model-facing browser tool;
//! - [`nanocodex_vm`] owns the private VM, network helper, immutable root disk,
//!   and provider-neutral egress capability.
//!
//! Every spawn reflinks an immutable ext4 template, runs headed Chromium as an
//! unprivileged guest user under Xvfb, and forwards CDP only to a random host
//! loopback port. [`BrowserVm::shutdown`] closes the controller and reaps
//! Chromium, gvproxy, the VMM, and the disposable disk together.
//!
//! # Start an isolated browser
//!
//! ```no_run
//! use nanocodex_browser::{BrowserAction, BrowserActionResult, vm::BrowserVm};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let browser = BrowserVm::builder(
//!     ".cache/browser/rootfs.ext4",
//!     "target/debug/nanocodex",
//!     ".cache/bin/gvproxy",
//! )
//! .vmm_args(["vm-run-config", "--config"])
//! .spawn()
//! .await?;
//!
//! let opened = browser
//!     .browser()
//!     .execute(BrowserAction::Open {
//!         url: "https://example.com/".to_owned(),
//!     })
//!     .await?;
//! assert!(matches!(opened, BrowserActionResult::Action { .. }));
//!
//! let browser_tool = browser.tool();
//! # let _ = browser_tool;
//! browser.shutdown().await?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Seek, SeekFrom},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use crate::{Browser, BrowserBuildError, BrowserBuilder, BrowserError, BrowserTool};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex_vm::host::{
    EgressLease, GUEST_EGRESS_ROOT, GuestCommand, Gvproxy, Network, PrivateVmProcessConfig,
    SharedDirectory, VmConfig, VmProcessConfig,
};
use serde::Deserialize;
use tempfile::TempDir;
use thiserror::Error;
use tokio::{process::Child, time};
use tracing::{Instrument, info, info_span, warn};
use url::Url;

const GUEST_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 127, 2);
const GVPROXY_HOST_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 127, 254);
const GUEST_CDP_RELAY_PORT: u16 = 9_223;
const DEFAULT_CPUS: u8 = 2;
const DEFAULT_MEMORY_MIB: u32 = 2_048;
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_ERROR_LOG_BYTES: u64 = 64 * 1_024;
const SHORT_TEMP_DIRECTORY: &str = "/tmp";
const EGRESS_FILES_TAG: &str = "ncx-egress";
const NO_EGRESS_FILES_ARGUMENT: &str = "-";
const EGRESS_FILES_HOST_DIRECTORY: &str = "egress-files";
const BROWSER_INIT_PROGRAM: &str = "/usr/local/bin/nanocodex-browser-vm-init";
const BROWSER_PROXY_ENVIRONMENT: &str = "NANOCODEX_BROWSER_PROXY_SERVER";
const BROWSER_PROXY_USERNAME_ENVIRONMENT: &str = "NANOCODEX_BROWSER_PROXY_USERNAME_B64";
const BROWSER_PROXY_PASSWORD_ENVIRONMENT: &str = "NANOCODEX_BROWSER_PROXY_PASSWORD_B64";
const BROWSER_INTERNAL_ENVIRONMENT: [&str; 3] = [
    BROWSER_PROXY_ENVIRONMENT,
    BROWSER_PROXY_USERNAME_ENVIRONMENT,
    BROWSER_PROXY_PASSWORD_ENVIRONMENT,
];
const PROXY_ENVIRONMENT: [&str; 4] = ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"];

#[derive(Debug, Error)]
/// Failure to configure, start, connect to, or stop a browser VM.
pub enum BrowserVmError {
    /// A numeric or duration setting is outside its supported domain.
    #[error("browser VM configuration is invalid: {0}")]
    InvalidConfig(&'static str),

    /// The immutable browser image does not identify a file.
    #[error("browser VM root disk is not a file: {0}")]
    InvalidRootDisk(PathBuf),

    /// The dedicated VMM entry point does not identify a file.
    #[error("browser VM VMM executable is not a file: {0}")]
    InvalidVmm(PathBuf),

    /// The gvproxy executable does not identify a file.
    #[error("browser VM gvproxy executable is not a file: {0}")]
    InvalidGvproxy(PathBuf),

    /// The configured libkrun firmware location is not a directory.
    #[error("browser VM firmware directory is not a directory: {0}")]
    InvalidFirmwareDirectory(PathBuf),

    /// A headed browser requires a private guest network.
    #[error("browser VM egress must request internet access")]
    InvalidEgressNetwork,

    /// A provider-owned read-only mount is unavailable.
    #[error("browser VM egress host mount is not a directory: {0}")]
    InvalidEgressMount(PathBuf),

    /// A provider mount cannot be placed at a relative guest path.
    #[error("browser VM egress guest mount must be absolute: {0}")]
    InvalidGuestMount(PathBuf),

    /// A configured proxy environment value was not a usable HTTP(S) URL.
    #[error("browser VM egress proxy `{0}` is not a valid HTTP(S) proxy URL")]
    InvalidProxyEnvironment(String),

    /// Proxy environment aliases resolve to different front proxies.
    #[error("browser VM egress proxy aliases must identify one front proxy")]
    ConflictingProxyEnvironment,

    /// A provider attempted to claim a browser VM internal proxy setting.
    #[error("browser VM egress cannot define browser-internal proxy settings")]
    ReservedProxyEnvironment,

    /// The disposable root disk could not be created.
    #[error("failed to create the browser VM's private root disk: {0}")]
    RootDisk(io::Error),

    /// Public egress files could not be staged for guest provisioning.
    #[error("failed to stage browser VM egress files: {0}")]
    EgressFiles(io::Error),

    /// A short-lived directory for Unix network sockets could not be created.
    #[error("failed to create the browser VM's short network directory: {0}")]
    NetworkDirectory(io::Error),

    /// A private host-loopback port could not be reserved.
    #[error("failed to reserve a private host CDP port: {0}")]
    ReservePort(io::Error),

    /// The dedicated VMM could not be launched or signalled.
    #[error("failed to spawn the browser VMM: {0}")]
    Spawn(io::Error),

    /// The VMM process could not be inspected or reaped.
    #[error("failed to inspect the browser VMM: {0}")]
    InspectVmm(io::Error),

    /// Chromium or its VMM exited before publishing CDP metadata.
    #[error("browser VMM exited before CDP became ready: {status}\n{log}")]
    EarlyExit {
        /// Exit status returned by the dedicated VMM.
        status: std::process::ExitStatus,
        /// Bounded tail of the VMM and browser log.
        log: String,
    },

    /// Chromium did not become reachable before the configured deadline.
    #[error("browser CDP endpoint {endpoint} was not ready within {timeout:?}\n{log}")]
    StartupTimeout {
        /// Private loopback HTTP endpoint that remained unavailable.
        endpoint: Box<Url>,
        /// Configured startup deadline.
        timeout: Duration,
        /// Bounded tail of the VMM and browser log.
        log: String,
    },

    /// A local CDP URL could not be constructed.
    #[error("failed to construct the browser CDP endpoint: {0}")]
    Endpoint(#[from] url::ParseError),

    /// Chromium returned malformed version metadata.
    #[error("browser CDP metadata was invalid: {0}")]
    CdpMetadata(#[from] serde_json::Error),

    /// Chromium returned a WebSocket endpoint that cannot be made private.
    #[error("browser returned an unusable WebSocket endpoint: {0}")]
    InvalidWebSocketEndpoint(Box<Url>),

    /// The bounded CDP readiness client could not be built or used.
    #[error("failed to construct the browser readiness client: {0}")]
    HttpClient(reqwest::Error),

    /// The VMM did not exit before the configured shutdown deadline.
    #[error("browser VMM did not exit within {0:?}")]
    ShutdownTimeout(Duration),

    /// The typed browser controller could not be configured.
    #[error(transparent)]
    BrowserBuild(#[from] BrowserBuildError),

    /// The typed browser controller could not connect, act, or close.
    #[error(transparent)]
    Browser(#[from] BrowserError),

    /// Both graceful browser shutdown and forced runtime cleanup failed.
    #[error("browser controller shutdown failed: {browser}; VM cleanup also failed: {runtime}")]
    CombinedShutdown {
        /// Controller-side shutdown failure.
        browser: Box<BrowserError>,
        /// VMM-side shutdown failure.
        runtime: Box<Self>,
    },

    /// The private gvproxy network could not be started or configured.
    #[error(transparent)]
    Network(#[from] nanocodex_vm::host::GvproxyError),

    /// The process-private VM configuration could not be serialized.
    #[error(transparent)]
    ProcessConfig(#[from] nanocodex_vm::host::VmProcessError),
}

/// Builder for one private headed Chromium VM.
pub struct BrowserVmBuilder {
    root_disk: PathBuf,
    vmm: PathBuf,
    vmm_arguments: Vec<OsString>,
    gvproxy: PathBuf,
    firmware_directory: Option<PathBuf>,
    cpus: u8,
    memory_mib: u32,
    startup_timeout: Duration,
    shutdown_timeout: Duration,
    egress: EgressLease,
    browser: BrowserBuilder,
}

impl BrowserVmBuilder {
    /// Creates a headed-browser VM builder from explicit runtime artifacts.
    pub fn new(
        root_disk: impl Into<PathBuf>,
        vmm: impl Into<PathBuf>,
        gvproxy: impl Into<PathBuf>,
    ) -> Self {
        Self {
            root_disk: root_disk.into(),
            vmm: vmm.into(),
            vmm_arguments: Vec::new(),
            gvproxy: gvproxy.into(),
            firmware_directory: None,
            cpus: DEFAULT_CPUS,
            memory_mib: DEFAULT_MEMORY_MIB,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
            egress: EgressLease::internet(),
            browser: Browser::builder(),
        }
    }

    /// Appends one argument before the process-private VM config path.
    ///
    /// For example, the repository binary is selected with
    /// `.vmm_args(["vm-run-config", "--config"])`. A dedicated VMM binary
    /// normally needs no preceding argument.
    #[must_use]
    pub fn vmm_arg(mut self, argument: impl Into<OsString>) -> Self {
        self.vmm_arguments.push(argument.into());
        self
    }

    /// Appends arguments before the process-private VM config path.
    #[must_use]
    pub fn vmm_args<I, A>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        self.vmm_arguments
            .extend(arguments.into_iter().map(Into::into));
        self
    }

    #[must_use]
    /// Adds the directory containing `libkrunfw` runtime libraries.
    pub fn firmware_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.firmware_directory = Some(directory.into());
        self
    }

    #[must_use]
    /// Sets the guest vCPU count.
    pub const fn cpus(mut self, cpus: u8) -> Self {
        self.cpus = cpus;
        self
    }

    #[must_use]
    /// Sets guest memory in mebibytes.
    pub const fn memory_mib(mut self, memory_mib: u32) -> Self {
        self.memory_mib = memory_mib;
        self
    }

    #[must_use]
    /// Bounds VM boot, Chromium startup, and private CDP forwarding.
    pub const fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Bounds graceful Chromium and VMM shutdown.
    #[must_use]
    pub const fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Supplies VM-facing proxy configuration, read-only CA mounts, and
    /// provider lifecycle guards. The browser owns its dedicated gvproxy
    /// transport, so the lease must request internet access rather than a
    /// caller-owned network socket.
    ///
    /// All HTTP proxy aliases must identify the same endpoint. A host-loopback
    /// endpoint is projected through gvproxy, its public CA is installed into
    /// Chromium's system and NSS trust, and credentials answer only proxy
    /// authentication challenges. Conflicts fail during [`Self::spawn`].
    #[must_use]
    pub fn egress(mut self, egress: EgressLease) -> Self {
        self.egress = egress;
        self
    }

    /// Supplies controller policy applied to Chromium inside the VM.
    ///
    /// Use this for React diagnostics, browser context, storage state, virtual
    /// passkeys, and model-level egress policy. The VM owns the CDP endpoint,
    /// so it replaces any endpoint already set on this builder. An explicit
    /// browser executable remains incompatible and is rejected during spawn.
    #[must_use]
    pub fn browser(mut self, browser: BrowserBuilder) -> Self {
        self.browser = browser;
        self
    }

    /// Clones the immutable root disk and starts one private headed Chromium VM.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, filesystem, process, network, readiness,
    /// or CDP metadata error. A failed start terminates its VMM and network
    /// helper before returning.
    pub async fn spawn(self) -> Result<BrowserVm, BrowserVmError> {
        let span = info_span!(
            target: "nanocodex_browser_vm",
            "browser_vm.spawn",
            browser_vm.root_disk = %self.root_disk.display(),
            browser_vm.cpus = self.cpus,
            browser_vm.memory_mib = self.memory_mib,
            browser_vm.egress_mounts = self.egress.guest_mounts().count(),
            browser_vm.egress_files = self.egress.guest_files().count(),
        );
        self.spawn_inner().instrument(span).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "spawn keeps validation and the single-owner VM/network/controller handoff linear"
    )]
    async fn spawn_inner(self) -> Result<BrowserVm, BrowserVmError> {
        validate_file(&self.root_disk, BrowserVmError::InvalidRootDisk)?;
        validate_file(&self.vmm, BrowserVmError::InvalidVmm)?;
        validate_file(&self.gvproxy, BrowserVmError::InvalidGvproxy)?;
        if self.cpus == 0 {
            return Err(BrowserVmError::InvalidConfig("CPU count must be nonzero"));
        }
        if self.memory_mib == 0 {
            return Err(BrowserVmError::InvalidConfig("memory must be nonzero"));
        }
        if self.startup_timeout.is_zero() {
            return Err(BrowserVmError::InvalidConfig(
                "startup timeout must be nonzero",
            ));
        }
        if self.shutdown_timeout.is_zero() {
            return Err(BrowserVmError::InvalidConfig(
                "shutdown timeout must be nonzero",
            ));
        }
        if let Some(firmware) = &self.firmware_directory
            && !firmware.is_dir()
        {
            return Err(BrowserVmError::InvalidFirmwareDirectory(firmware.clone()));
        }
        if self.egress.network() != &Network::Internet {
            return Err(BrowserVmError::InvalidEgressNetwork);
        }
        for mount in self.egress.guest_mounts() {
            if !mount.host_path().is_dir() {
                return Err(BrowserVmError::InvalidEgressMount(
                    mount.host_path().to_owned(),
                ));
            }
            if !mount.guest_path().is_absolute() {
                return Err(BrowserVmError::InvalidGuestMount(
                    mount.guest_path().to_owned(),
                ));
            }
        }

        let directory = tempfile::Builder::new()
            .prefix("nanocodex-browser-vm-")
            .tempdir()
            .map_err(BrowserVmError::RootDisk)?;
        let root_disk = directory.path().join("rootfs.ext4");
        reflink_copy::reflink_or_copy(&self.root_disk, &root_disk)
            .map_err(BrowserVmError::RootDisk)?;
        let egress_files = directory.path().join(EGRESS_FILES_HOST_DIRECTORY);
        stage_egress_files(&self.egress, &egress_files)?;
        let BrowserEgressProjection {
            guest_environment,
            browser_proxy,
        } = browser_proxy(&self.egress)?;

        let network_directory = tempfile::Builder::new()
            .prefix("ncx-net-")
            .tempdir_in(SHORT_TEMP_DIRECTORY)
            .map_err(BrowserVmError::NetworkDirectory)?;
        let network_log = directory.path().join("gvproxy.log");
        let network = Gvproxy::spawn(&self.gvproxy, network_directory.path(), &network_log)?;
        let local = reserve_loopback_address()?;
        let remote = SocketAddr::V4(SocketAddrV4::new(GUEST_ADDRESS, GUEST_CDP_RELAY_PORT));
        network.forward_tcp(local, remote)?;
        let cdp_http_endpoint = Url::parse(&format!("http://{local}"))?;

        let browser_log_path = directory.path().join("browser-vm.log");
        let browser_log = fs::File::create(&browser_log_path).map_err(BrowserVmError::Spawn)?;
        let browser_error_log = browser_log.try_clone().map_err(BrowserVmError::Spawn)?;
        let mut vm_config = VmConfig::ext4(&root_disk)
            .network(Network::gvproxy(network.network_socket()))
            .cpus(self.cpus)
            .memory_mib(self.memory_mib);
        let mut guest_command = GuestCommand::new(BROWSER_INIT_PROGRAM);
        if self.egress.guest_files().next().is_some() {
            vm_config = vm_config
                .shared_directory(SharedDirectory::read_only(EGRESS_FILES_TAG, &egress_files));
            guest_command = guest_command.arg(EGRESS_FILES_TAG);
        } else {
            guest_command = guest_command.arg(NO_EGRESS_FILES_ARGUMENT);
        }
        for mount in self.egress.guest_mounts() {
            vm_config = vm_config
                .shared_directory(SharedDirectory::read_only(mount.tag(), mount.host_path()));
            guest_command = guest_command
                .arg(mount.tag())
                .arg(mount.guest_path().as_os_str());
        }
        for (name, value) in guest_environment {
            guest_command = guest_command.env(name, value);
        }
        if let Some(proxy) = &browser_proxy {
            guest_command = guest_command.env(BROWSER_PROXY_ENVIRONMENT, proxy.server.as_str());
            if let Some(credentials) = &proxy.credentials {
                guest_command = guest_command
                    .env(
                        BROWSER_PROXY_USERNAME_ENVIRONMENT,
                        STANDARD.encode(credentials.username.as_bytes()),
                    )
                    .env(
                        BROWSER_PROXY_PASSWORD_ENVIRONMENT,
                        STANDARD.encode(credentials.password.as_bytes()),
                    );
            }
        }
        let process_config = VmProcessConfig::new(vm_config, guest_command).write_private()?;

        let mut command = tokio::process::Command::new(&self.vmm);
        command
            .env_clear()
            .args(&self.vmm_arguments)
            .arg(process_config.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(browser_log))
            .stderr(Stdio::from(browser_error_log))
            .kill_on_drop(true);
        if let Some(firmware) = self.firmware_directory {
            command.env("DYLD_LIBRARY_PATH", dynamic_library_path(&firmware));
        }
        let child = command.spawn().map_err(BrowserVmError::Spawn)?;
        let mut runtime = BrowserVmRuntime {
            child,
            reaped: false,
            _network: network,
            _network_directory: network_directory,
            cdp_endpoint: cdp_http_endpoint,
            root_disk,
            log: browser_log_path,
            network_log,
            _egress: self.egress,
            _process_config: process_config,
            _directory: directory,
            shutdown_timeout: self.shutdown_timeout,
        };
        let started_at = Instant::now();
        runtime.cdp_endpoint = runtime
            .wait_until_ready(self.startup_timeout, local)
            .await?;
        let browser = self
            .browser
            .cdp_endpoint(runtime.cdp_endpoint.clone())
            .build()?;
        browser.start().await?;
        let startup_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        info!(
            target: "nanocodex_browser_vm",
            startup_ms,
            "browser VM and typed controller are ready"
        );
        Ok(BrowserVm { browser, runtime })
    }
}

struct BrowserVmRuntime {
    child: Child,
    reaped: bool,
    _network: Gvproxy,
    _network_directory: TempDir,
    cdp_endpoint: Url,
    root_disk: PathBuf,
    log: PathBuf,
    network_log: PathBuf,
    _egress: EgressLease,
    _process_config: PrivateVmProcessConfig,
    _directory: TempDir,
    shutdown_timeout: Duration,
}

impl BrowserVmRuntime {
    async fn shutdown(&mut self) -> Result<(), BrowserVmError> {
        if self
            .child
            .try_wait()
            .map_err(BrowserVmError::InspectVmm)?
            .is_some()
        {
            self.reaped = true;
            return Ok(());
        }
        self.child.start_kill().map_err(BrowserVmError::Spawn)?;
        match time::timeout(self.shutdown_timeout, self.child.wait()).await {
            Ok(Ok(_)) => {
                self.reaped = true;
                Ok(())
            }
            Ok(Err(error)) => Err(BrowserVmError::InspectVmm(error)),
            Err(_) => Err(BrowserVmError::ShutdownTimeout(self.shutdown_timeout)),
        }
    }

    async fn wait_until_ready(
        &mut self,
        timeout: Duration,
        local: SocketAddr,
    ) -> Result<Url, BrowserVmError> {
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .map_err(BrowserVmError::HttpClient)?;
        let version = self.cdp_endpoint.join("/json/version")?;
        let started_at = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().map_err(BrowserVmError::InspectVmm)? {
                return Err(BrowserVmError::EarlyExit {
                    status,
                    log: read_startup_logs(&self.log, &self.network_log),
                });
            }
            if let Ok(response) = client.get(version.clone()).send().await
                && response.status().is_success()
            {
                let metadata = serde_json::from_str::<CdpVersion>(
                    &response.text().await.map_err(BrowserVmError::HttpClient)?,
                )?;
                return local_websocket_endpoint(
                    Url::parse(&metadata.web_socket_debugger_url)?,
                    local,
                );
            }
            if started_at.elapsed() >= timeout {
                return Err(BrowserVmError::StartupTimeout {
                    endpoint: Box::new(self.cdp_endpoint.clone()),
                    timeout,
                    log: read_startup_logs(&self.log, &self.network_log),
                });
            }
            time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// One typed browser controller and its isolated headed Chromium VM.
pub struct BrowserVm {
    browser: Browser,
    runtime: BrowserVmRuntime,
}

impl BrowserVm {
    /// Starts a builder from explicit immutable-image and runtime artifacts.
    #[must_use]
    pub fn builder(
        root_disk: impl Into<PathBuf>,
        vmm: impl Into<PathBuf>,
        gvproxy: impl Into<PathBuf>,
    ) -> BrowserVmBuilder {
        BrowserVmBuilder::new(root_disk, vmm, gvproxy)
    }

    /// Returns the cloneable typed controller connected to this VM.
    #[must_use]
    pub const fn browser(&self) -> &Browser {
        &self.browser
    }

    /// Returns an ordinary Nanocodex tool connected to this VM.
    #[must_use]
    pub fn tool(&self) -> BrowserTool {
        BrowserTool::from_browser(self.browser.clone())
    }

    /// Returns the private host-loopback WebSocket endpoint.
    ///
    /// Normal consumers use [`Self::browser`] or [`Self::tool`]. This endpoint
    /// remains available for dedicated `DevTools` integrations and benchmarks.
    #[must_use]
    pub const fn cdp_endpoint(&self) -> &Url {
        &self.runtime.cdp_endpoint
    }

    /// Returns the attempt-private copy-on-write root disk.
    #[must_use]
    pub fn root_disk(&self) -> &Path {
        &self.runtime.root_disk
    }

    /// Closes Chromium and reaps the VMM, network helper, and temporary disk.
    ///
    /// # Errors
    ///
    /// Returns a typed controller or runtime error. If both graceful browser
    /// shutdown and forced VM cleanup fail, both failures are retained.
    pub async fn shutdown(self) -> Result<(), BrowserVmError> {
        let Self {
            browser,
            mut runtime,
        } = self;
        let span = info_span!(target: "nanocodex_browser_vm", "browser_vm.shutdown");
        async move {
            let browser_result = browser.close().await;
            let runtime_result = runtime.shutdown().await;
            match (browser_result, runtime_result) {
                (Ok(()), Ok(())) => {
                    info!(
                        target: "nanocodex_browser_vm",
                        "browser controller and VM shut down"
                    );
                    Ok(())
                }
                (Err(browser), Ok(())) => Err(BrowserVmError::Browser(browser)),
                (Ok(()), Err(runtime)) => Err(runtime),
                (Err(browser), Err(runtime)) => Err(BrowserVmError::CombinedShutdown {
                    browser: Box::new(browser),
                    runtime: Box::new(runtime),
                }),
            }
        }
        .instrument(span)
        .await
    }
}

#[derive(Deserialize)]
struct CdpVersion {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

impl Drop for BrowserVmRuntime {
    fn drop(&mut self) {
        if !self.reaped
            && let Err(error) = self.child.start_kill()
        {
            warn!(
                target: "nanocodex_browser_vm",
                %error,
                "failed to signal dropped browser VM"
            );
        }
    }
}

fn reserve_loopback_address() -> Result<SocketAddr, BrowserVmError> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .map_err(BrowserVmError::ReservePort)?;
    listener.local_addr().map_err(BrowserVmError::ReservePort)
}

fn validate_file(
    path: &Path,
    error: impl FnOnce(PathBuf) -> BrowserVmError,
) -> Result<(), BrowserVmError> {
    path.is_file()
        .then_some(())
        .ok_or_else(|| error(path.to_path_buf()))
}

struct BrowserProxy {
    server: String,
    credentials: Option<BrowserProxyCredentials>,
}

struct BrowserProxyCredentials {
    username: String,
    password: String,
}

struct BrowserEgressProjection {
    guest_environment: Vec<(String, String)>,
    browser_proxy: Option<BrowserProxy>,
}

fn browser_proxy(egress: &EgressLease) -> Result<BrowserEgressProjection, BrowserVmError> {
    if BROWSER_INTERNAL_ENVIRONMENT
        .iter()
        .any(|name| egress.guest_environment().contains_key(*name))
    {
        return Err(BrowserVmError::ReservedProxyEnvironment);
    }
    let mut environment = Vec::with_capacity(egress.guest_environment().len());
    let mut selected = None::<Url>;
    for (name, value) in egress.guest_environment() {
        if PROXY_ENVIRONMENT.contains(&name.as_str()) {
            let url = gvproxy_proxy_url(name, value)?;
            if selected.as_ref().is_some_and(|selected| selected != &url) {
                return Err(BrowserVmError::ConflictingProxyEnvironment);
            }
            selected = Some(url.clone());
            environment.push((name.clone(), url.to_string()));
        } else {
            environment.push((name.clone(), value.clone()));
        }
    }
    let Some(mut url) = selected else {
        return Ok(BrowserEgressProjection {
            guest_environment: environment,
            browser_proxy: None,
        });
    };
    let username = decode_proxy_credential(url.username())?;
    let password = url.password().map(decode_proxy_credential).transpose()?;
    url.set_username("")
        .map_err(|()| BrowserVmError::InvalidProxyEnvironment("proxy".to_owned()))?;
    url.set_password(None)
        .map_err(|()| BrowserVmError::InvalidProxyEnvironment("proxy".to_owned()))?;
    let credentials =
        (!username.is_empty() || password.is_some()).then(|| BrowserProxyCredentials {
            username,
            password: password.unwrap_or_default(),
        });
    Ok(BrowserEgressProjection {
        guest_environment: environment,
        browser_proxy: Some(BrowserProxy {
            server: url[..url::Position::BeforePath].to_owned(),
            credentials,
        }),
    })
}

fn decode_proxy_credential(value: &str) -> Result<String, BrowserVmError> {
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| BrowserVmError::InvalidProxyEnvironment("proxy credentials".to_owned()))
}

fn gvproxy_proxy_url(name: &str, value: &str) -> Result<Url, BrowserVmError> {
    let mut url =
        Url::parse(value).map_err(|_| BrowserVmError::InvalidProxyEnvironment(name.to_owned()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BrowserVmError::InvalidProxyEnvironment(name.to_owned()));
    }
    let loopback = url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
        || url
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
    if loopback {
        url.set_host(Some(&GVPROXY_HOST_ADDRESS.to_string()))
            .map_err(|_| BrowserVmError::InvalidProxyEnvironment(name.to_owned()))?;
    }
    Ok(url)
}

fn stage_egress_files(egress: &EgressLease, directory: &Path) -> Result<(), BrowserVmError> {
    fs::create_dir_all(directory).map_err(BrowserVmError::EgressFiles)?;
    for file in egress.guest_files() {
        let relative = file
            .guest_path()
            .strip_prefix(GUEST_EGRESS_ROOT)
            .map_err(|error| {
                BrowserVmError::EgressFiles(io::Error::new(io::ErrorKind::InvalidInput, error))
            })?;
        let destination = directory.join(relative);
        let parent = destination.parent().ok_or_else(|| {
            BrowserVmError::EgressFiles(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "guest egress file has no parent: {}",
                    file.guest_path().display()
                ),
            ))
        })?;
        fs::create_dir_all(parent).map_err(BrowserVmError::EgressFiles)?;
        fs::write(&destination, file.contents()).map_err(BrowserVmError::EgressFiles)?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(file.mode()))
            .map_err(BrowserVmError::EgressFiles)?;
    }
    Ok(())
}

fn dynamic_library_path(directory: &Path) -> OsString {
    let mut value = directory.as_os_str().to_owned();
    if let Some(existing) = std::env::var_os("DYLD_LIBRARY_PATH")
        && !existing.is_empty()
    {
        value.push(":");
        value.push(existing);
    }
    value
}

fn local_websocket_endpoint(mut endpoint: Url, local: SocketAddr) -> Result<Url, BrowserVmError> {
    endpoint
        .set_host(Some(&local.ip().to_string()))
        .map_err(|_| BrowserVmError::InvalidWebSocketEndpoint(Box::new(endpoint.clone())))?;
    endpoint
        .set_port(Some(local.port()))
        .map_err(|()| BrowserVmError::InvalidWebSocketEndpoint(Box::new(endpoint.clone())))?;
    Ok(endpoint)
}

fn read_log(path: &Path) -> String {
    read_log_inner(path)
        .unwrap_or_else(|error| format!("failed to read {}: {error}", path.display()))
}

fn read_startup_logs(vmm: &Path, network: &Path) -> String {
    format!(
        "VMM/browser log:\n{}\n\ngvproxy log:\n{}",
        read_log(vmm),
        read_log(network)
    )
}

fn read_log_inner(path: &Path) -> Result<String, io::Error> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(MAX_ERROR_LOG_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(usize::try_from(length - start).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    let log = String::from_utf8_lossy(&bytes);
    let log = log.trim();
    if start == 0 {
        Ok(log.to_owned())
    } else {
        Ok(format!("[earlier browser VM log bytes omitted]\n{log}"))
    }
}

#[cfg(test)]
mod tests {
    use nanocodex_vm::host::EgressFile;

    use super::*;

    #[test]
    fn defaults_do_not_launch_headless_or_enable_automation() {
        let script = include_str!("../image/browser-vm-init");
        assert!(!script.contains("--headless"));
        assert!(!script.contains("--enable-automation"));
        assert!(script.contains("Xvfb :99"));
        assert!(script.contains("socat TCP-LISTEN:9223"));
        assert!(script.contains("--remote-debugging-port=9222"));
        assert!(script.contains("--proxy-server=$NANOCODEX_BROWSER_PROXY_SERVER"));
        assert!(script.contains("--proxy-bypass-list=<-loopback>"));
        assert!(script.contains("update-ca-certificates"));
        assert!(script.contains("certutil -A"));
        assert!(script.contains("details.isProxy"));
        assert!(script.contains("--load-extension=$NANOCODEX_BROWSER_PROXY_EXTENSION"));
        assert!(script.contains("files_tag=$1"));
        assert!(script.contains("mount -t virtiofs -o ro \"$files_tag\""));
        assert!(!script.contains("mount -t virtiofs -o ro nanocodex-egress-files"));
        assert!(!BROWSER_INIT_PROGRAM.contains('"'));
    }

    #[test]
    fn startup_errors_retain_only_a_bounded_log_tail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("browser.log");
        let prefix = "x".repeat(usize::try_from(MAX_ERROR_LOG_BYTES).unwrap());
        fs::write(&path, format!("{prefix}discarded\nlast useful line\n")).unwrap();

        let log = read_log(&path);

        assert!(log.starts_with("[earlier browser VM log bytes omitted]\n"));
        assert!(log.ends_with("last useful line"));
        assert!(log.len() <= usize::try_from(MAX_ERROR_LOG_BYTES).unwrap() + 64);
    }

    #[test]
    fn egress_files_keep_exact_contents_and_permissions() {
        let mut egress = EgressLease::internet();
        egress
            .insert_file(EgressFile::new(
                "/tmp/nanocodex/egress/provider/ca.pem",
                b"complete-ca-bundle".to_vec(),
                0o640,
            ))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();

        stage_egress_files(&egress, directory.path()).unwrap();

        let staged = directory.path().join("provider/ca.pem");
        assert_eq!(fs::read(&staged).unwrap(), b"complete-ca-bundle");
        assert_eq!(
            fs::metadata(staged).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn builder_retains_explicit_vmm_entrypoint_arguments() {
        let builder = BrowserVmBuilder::new("/images/browser.ext4", "/bin/vmm", "/bin/gvproxy")
            .vmm_args(["vm", "run-config", "--config"]);

        assert_eq!(
            builder.vmm_arguments,
            ["vm", "run-config", "--config"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn remote_cdp_metadata_is_rewritten_to_private_loopback() {
        let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 41_337));
        let endpoint = local_websocket_endpoint(
            Url::parse("ws://192.168.127.2:9222/devtools/browser/session").unwrap(),
            local,
        )
        .unwrap();

        assert_eq!(
            endpoint.as_str(),
            "ws://127.0.0.1:41337/devtools/browser/session"
        );
    }

    #[test]
    fn loopback_front_proxy_is_projected_through_the_gvproxy_host_address() {
        let mut egress = EgressLease::internet();
        for name in PROXY_ENVIRONMENT {
            egress
                .insert_environment(name, "http://nanocodex:host-capability@127.0.0.1:41337")
                .unwrap();
        }

        let projection = browser_proxy(&egress).unwrap();
        let browser = projection.browser_proxy.unwrap();

        assert_eq!(
            projection
                .guest_environment
                .iter()
                .find(|(name, _)| name == "HTTPS_PROXY")
                .map(|(_, value)| value.as_str()),
            Some("http://nanocodex:host-capability@192.168.127.254:41337/")
        );
        assert_eq!(browser.server, "http://192.168.127.254:41337");
        let credentials = browser.credentials.unwrap();
        assert_eq!(credentials.username, "nanocodex");
        assert_eq!(credentials.password, "host-capability");
    }

    #[test]
    fn proxy_credentials_are_percent_decoded_before_browser_projection() {
        let mut egress = EgressLease::internet();
        egress
            .insert_environment(
                "HTTPS_PROXY",
                "http://name%40example.test:p%40ss%3Aword@127.0.0.1:41337",
            )
            .unwrap();

        let projection = browser_proxy(&egress).unwrap();
        let credentials = projection.browser_proxy.unwrap().credentials.unwrap();

        assert_eq!(credentials.username, "name@example.test");
        assert_eq!(credentials.password, "p@ss:word");
    }

    #[test]
    fn conflicting_front_proxy_aliases_fail_closed() {
        let mut egress = EgressLease::internet();
        egress
            .insert_environment("HTTP_PROXY", "http://127.0.0.1:4100")
            .unwrap();
        egress
            .insert_environment("HTTPS_PROXY", "http://127.0.0.1:4200")
            .unwrap();

        assert!(matches!(
            browser_proxy(&egress),
            Err(BrowserVmError::ConflictingProxyEnvironment)
        ));
    }
}
