//! Configured harness support for one ordinary evaluator coordinate.
//!
//! This module contains no pairing, sweep, or comparison policy. It only
//! adapts one pinned executable to the evaluator's existing VM and
//! verifier lifecycle.

use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    future::Future,
    io::{self, Read},
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use nanocodex_agent::{NanocodexBuilder, Thinking};
use nanocodex_oai_api::Model;
use nanocodex_vm::host::Gvproxy;

use crate::{
    EvalAttemptOutcome, Evaluator, HarnessCommandOutput, HarnessCommandRunner,
    HarnessCommandRunnerError, HarnessCommandStatus, HarnessExec, ResponsesCaptureProxy,
    ResponsesCaptureProxyConfig, Task,
    evaluator::{AttemptAgent, EvalAttempt},
    harness_exec::project_harness_atif,
    vm::{
        VmAttempt, VmAttemptError, VmBackend, VmCommand, VmEnvironment, VmResources,
        VmToolSessionError, VmToolSessionHandle,
    },
};

type BoxError = Box<dyn Error + Send + Sync + 'static>;
type InternalResult<T, E = BoxError> = std::result::Result<T, E>;

macro_rules! harness_error {
    ($($argument:tt)*) => {
        Box::new(io::Error::other(format!($($argument)*))) as BoxError
    };
}

const HARNESS_CAPTURE_PROXY_API_UPSTREAM: &str = "https://api.openai.com/v1";
const HARNESS_CAPTURE_PROXY_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const HARNESS_API_EXCHANGES_FILENAME: &str = "api-exchanges.jsonl";
const DEFAULT_HARNESS_HOME: &str = "/run/nanocodex-harness-home";
const DEFAULT_HARNESS_AUTH_FILE: &str = "/run/nanocodex-harness-home/auth.json";
const DEFAULT_HARNESS_API_KEY_ENVIRONMENT: &str = "OPENAI_API_KEY";
const HARNESS_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const CAPTURE_ONLY_GUEST_RUNTIME: &str = "/usr/local/bin/nanocodex-vm-capture";
#[cfg(target_arch = "aarch64")]
const VM_GUEST_TARGET: &str = "aarch64-unknown-linux-musl";
#[cfg(target_arch = "x86_64")]
const VM_GUEST_TARGET: &str = "x86_64-unknown-linux-musl";
#[cfg(target_arch = "aarch64")]
const VM_GUEST_ELF_MACHINE: u16 = 183;
#[cfg(target_arch = "x86_64")]
const VM_GUEST_ELF_MACHINE: u16 = 62;

mod sandbox;
use sandbox::*;

/// Authentication material forwarded to one pinned external harness.
#[derive(Clone)]
pub struct HarnessAuth {
    kind: HarnessAuthKind,
}

#[derive(Clone)]
enum HarnessAuthKind {
    ApiKey(Arc<str>),
    AccessToken(Arc<str>),
    AuthFile(PathBuf),
}

#[derive(Clone)]
struct HarnessGuestConfig {
    environment: Vec<(String, String)>,
    home: String,
    auth_file: String,
    api_key_environment: String,
    api_upstream: Option<String>,
}

impl HarnessAuth {
    /// Uses an OpenAI API key in the harness guest.
    #[must_use]
    pub fn api_key(api_key: impl Into<Arc<str>>) -> Self {
        Self {
            kind: HarnessAuthKind::ApiKey(api_key.into()),
        }
    }

    /// Uses a persistent ChatGPT access token in the harness guest.
    #[must_use]
    pub fn access_token(access_token: impl Into<Arc<str>>) -> Self {
        Self {
            kind: HarnessAuthKind::AccessToken(access_token.into()),
        }
    }

    /// Copies one credential file into the generic harness home.
    #[must_use]
    pub fn auth_file(path: impl Into<PathBuf>) -> Self {
        Self {
            kind: HarnessAuthKind::AuthFile(path.into()),
        }
    }
}

/// One configured external harness coordinate.
pub struct Harness {
    nanocodex: NanocodexBuilder,
    task: Task,
    command: PathBuf,
    guest_command: String,
    auth: HarnessAuth,
    resources: VmResources,
    model: Model,
    thinking: Thinking,
    web_search: bool,
    output: PathBuf,
    guest_memory_mb: u64,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    home: String,
    auth_file: String,
    api_key_environment: String,
    api_upstream: Option<String>,
    version: String,
    name: String,
    verifier_environment: BTreeMap<String, String>,
}

impl Harness {
    /// Creates a harness adapter from its required immutable components.
    #[must_use]
    pub fn new(
        nanocodex: NanocodexBuilder,
        task: Task,
        command: impl Into<PathBuf>,
        guest_command: impl Into<String>,
        auth: HarnessAuth,
        resources: VmResources,
    ) -> Self {
        Self {
            nanocodex,
            task,
            command: command.into(),
            guest_command: guest_command.into(),
            auth,
            resources,
            model: Model::default(),
            thinking: Thinking::default(),
            web_search: false,
            output: PathBuf::from(".nanocodex/evals"),
            guest_memory_mb: 512,
            arguments: Vec::new(),
            environment: Vec::new(),
            home: DEFAULT_HARNESS_HOME.to_owned(),
            auth_file: DEFAULT_HARNESS_AUTH_FILE.to_owned(),
            api_key_environment: DEFAULT_HARNESS_API_KEY_ENVIRONMENT.to_owned(),
            api_upstream: None,
            version: "configured".to_owned(),
            name: "harness".to_owned(),
            verifier_environment: BTreeMap::new(),
        }
    }

    /// Pins the model used by this coordinate.
    #[must_use]
    pub const fn model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    /// Pins the reasoning effort used by this coordinate.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = thinking;
        self
    }

    /// Selects whether standalone web search is available.
    #[must_use]
    pub const fn web_search(mut self, enabled: bool) -> Self {
        self.web_search = enabled;
        self
    }

    /// Selects the evaluator-owned artifact parent.
    #[must_use]
    pub fn output_directory(mut self, output: impl Into<PathBuf>) -> Self {
        self.output = output.into();
        self
    }

    /// Caps RAM for the harness guest.
    #[must_use]
    pub const fn guest_memory_mb(mut self, memory_mb: u64) -> Self {
        self.guest_memory_mb = memory_mb;
        self
    }

    /// Configures the guest argument template for this harness binary.
    #[must_use]
    pub fn arguments(mut self, arguments: Vec<String>) -> Self {
        self.arguments = arguments;
        self
    }

    /// Adds harness-specific guest environment variables.
    #[must_use]
    pub fn environment(mut self, environment: Vec<(String, String)>) -> Self {
        self.environment = environment;
        self
    }

    /// Adds run-scoped values visible only to verifier commands.
    #[must_use]
    pub fn verifier_environment(
        mut self,
        environment: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.verifier_environment.extend(environment);
        self
    }

    /// Configures neutral and agent-specific guest credential locations.
    #[must_use]
    pub fn credentials(
        mut self,
        home: impl Into<String>,
        auth_file: impl Into<String>,
        api_key_environment: impl Into<String>,
    ) -> Self {
        self.home = home.into();
        self.auth_file = auth_file.into();
        self.api_key_environment = api_key_environment.into();
        self
    }

    /// Overrides the OpenAI-compatible upstream used by the capture proxy.
    #[must_use]
    pub fn api_upstream(mut self, upstream: Option<String>) -> Self {
        self.api_upstream = upstream;
        self
    }

    /// Records the profile-pinned semantic harness version in trajectories.
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Records the profile-visible harness name in retained trajectories.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Prepares an ordinary one-attempt evaluator using the configured harness.
    pub async fn prepare(self) -> Result<PreparedHarness, HarnessError> {
        if self.guest_memory_mb == 0 {
            return Err(HarnessError::message("guest memory must be non-zero"));
        }
        if !Path::new(&self.guest_command).is_absolute() {
            return Err(HarnessError::message(format!(
                "guest command must be an absolute path: {}",
                self.guest_command
            )));
        }
        fs::create_dir_all(&self.output).map_err(HarnessError::from_error)?;
        let mut header = [0_u8; 20];
        fs::File::open(&self.command)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(HarnessError::from_error)?;
        validate_vm_guest_elf(&header, &self.command).map_err(HarnessError::from_box)?;
        let resources = prepare_harness_vm_resources(
            &self.task,
            &self.resources,
            self.guest_memory_mb,
            self.web_search,
            &self.verifier_environment,
        )
        .await
        .map_err(HarnessError::from_box)?;
        let command = HarnessExec::new(&self.command, self.model.as_str(), self.thinking.as_str())
            .map_err(HarnessError::from_error)?
            .web_search(self.web_search)
            .arguments(self.arguments);
        let backend = resources.backend();
        let resources = Arc::new(resources);
        let auth = self.auth;
        let version = self.version;
        let name = self.name;
        let guest_command = self.guest_command;
        let guest = HarnessGuestConfig {
            environment: self.environment,
            home: self.home,
            auth_file: self.auth_file,
            api_key_environment: self.api_key_environment,
            api_upstream: self.api_upstream,
        };
        let evaluator = Evaluator::new_builder(self.nanocodex)
            .output_directory(self.output)
            .vm_with(backend, move |attempt, _builder, runtime| {
                resources.harness_attempt(
                    runtime,
                    attempt,
                    command.clone(),
                    guest_command.clone(),
                    auth.clone(),
                    guest.clone(),
                )
            })
            .build()
            .map_err(HarnessError::from_error)?;
        Ok(PreparedHarness {
            evaluator,
            version,
            name,
        })
    }
}

/// A prepared one-coordinate harness over the ordinary evaluator lifecycle.
pub struct PreparedHarness {
    evaluator: Evaluator,
    version: String,
    name: String,
}

impl PreparedHarness {
    /// Returns the evaluator that executes and verifies this coordinate.
    #[must_use]
    pub const fn evaluator(&self) -> &Evaluator {
        &self.evaluator
    }

    /// Projects the retained harness JSONL into the ordinary trajectory file.
    pub async fn retain_trajectory(
        &self,
        outcome: &EvalAttemptOutcome,
    ) -> Result<(), HarnessError> {
        let Some(result) = outcome.agent().cloned() else {
            return Ok(());
        };
        let prompt = outcome.task().prompt().to_owned();
        let events = outcome
            .artifacts()
            .directory
            .join("agent/harness-events.jsonl");
        let trajectory_path = outcome.artifacts().directory.join("agent/trajectory.json");
        let version = self.version.clone();
        let name = self.name.clone();
        tokio::task::spawn_blocking(move || {
            let trajectory = project_harness_atif(&events, &prompt, &result, &name, &version)?;
            write_json_atomic(&trajectory_path, &trajectory)
        })
        .await
        .map_err(HarnessError::from_error)?
        .map_err(HarnessError::from_box)
    }

    /// Returns the attempt artifact parent.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.evaluator.directory()
    }
}

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> InternalResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| harness_error!("trajectory path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    use std::io::Write as _;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Failure to configure one external harness coordinate.
#[derive(Debug, thiserror::Error)]
#[error("failed to prepare external harness: {source}")]
pub struct HarnessError {
    #[source]
    source: BoxError,
}

impl HarnessError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            source: Box::new(io::Error::other(message.into())),
        }
    }

    fn from_error(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Box::new(source),
        }
    }

    fn from_box(source: BoxError) -> Self {
        Self { source }
    }
}
