use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use nanocodex_oai_api::{Prompt, PromptMessage};
use serde::{Deserialize, Serialize};

use crate::digest::TaskPackage;

const TASK_CONFIG: &str = "task.toml";
const TASK_INSTRUCTION: &str = "instruction.md";
const TASK_TRANSCRIPT: &str = "transcript.json";
const TASK_ENVIRONMENT: &str = "environment";
const VERIFIER_SCRIPT: &str = "tests/test.sh";
const PRE_ARTIFACTS_SCRIPT: &str = "pre_artifacts.sh";

/// One immutable benchmark task loaded from a Terminal-Bench task directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    root: PathBuf,
    content_digest: Box<str>,
    package: Arc<TaskPackage>,
    dataset: Option<Box<str>>,
    dataset_revision: Option<Box<str>>,
    name: Box<str>,
    description: Box<str>,
    prompt: Box<str>,
    transcript: Vec<PromptMessage>,
    prompt_chars: u64,
    benchmark_prompt_chars: Option<u64>,
    benchmark_case_type: Option<Box<str>>,
    agent_instructions: Option<Box<str>>,
    image: OciImage,
    agent_timeout: Duration,
    verifier: Verifier,
    artifacts: Vec<TaskArtifact>,
    output: TaskOutput,
    resources: Resources,
    network: NetworkPolicy,
    environment: BTreeMap<String, String>,
    requires_compose: bool,
}

/// OCI image declared by a benchmark task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OciImage {
    reference: Box<str>,
}

/// Verifier recipe loaded from a task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verifier {
    script: PathBuf,
    timeout: Duration,
    environment: BTreeMap<String, String>,
    environment_mode: VerifierEnvironmentMode,
    collect: Vec<VerifierCollect>,
    scoring_policy: ScoringPolicy,
    network: NetworkPolicy,
}

/// Binary classification applied to benchmark-owned numeric rewards.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScoringPolicy {
    /// Every named reward must be strictly positive.
    #[default]
    AllRewardsPositive,
    /// Every named reward must be exactly one.
    AllRewardsOne,
}

/// Whether verification reuses the agent environment or a separate image.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifierEnvironmentMode {
    /// Run verification in the mutated agent environment.
    #[default]
    Same,
    /// Run verification in the task's dedicated `tests/Dockerfile` image.
    Separate,
}

/// One post-verifier artifact collection command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerifierCollect {
    command: Box<str>,
}

/// One guest artifact retained after agent execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TaskArtifact {
    source: PathBuf,
    exclude: Vec<PathBuf>,
    service: Option<Box<str>>,
}

/// Candidate value made available to the canonical verifier.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutput {
    /// The verifier consumes mutations in the task workspace.
    #[default]
    Workspace,
    /// The verifier consumes the final assistant message at `answer.txt` in
    /// the task workspace.
    FinalMessage,
}

/// Task-declared resource requirements used by admission and VM sizing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resources {
    /// Virtual CPU count.
    pub cpus: u32,
    /// Required memory in mebibytes.
    pub memory_mb: u64,
    /// Required storage in mebibytes.
    pub storage_mb: u64,
    /// Required GPU count.
    pub gpus: u32,
}

/// Task network policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkPolicy {
    /// The task may reach public network destinations.
    Public,
    /// The task must run without a network device.
    Disabled,
}

/// Failure to load or validate an immutable task directory.
#[derive(Debug, thiserror::Error)]
pub enum TaskLoadError {
    /// The task root could not be canonicalized.
    #[error("failed to resolve task directory {path}: {source}")]
    ResolveDirectory {
        /// Requested task path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },

    /// A task file could not be read.
    #[error("failed to read task file {path}: {source}")]
    Read {
        /// File that could not be read.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },

    /// `task.toml` was not valid TOML.
    #[error("failed to parse task configuration {path}: {source}")]
    Parse {
        /// Configuration path.
        path: PathBuf,
        /// TOML parser failure.
        #[source]
        source: toml::de::Error,
    },

    /// `transcript.json` was not valid JSON.
    #[error("failed to parse task transcript {path}: {source}")]
    TranscriptParse {
        /// Transcript path.
        path: PathBuf,
        /// JSON parser failure.
        #[source]
        source: serde_json::Error,
    },

    /// The manifest declares an unsupported schema revision.
    #[error("unsupported task schema version {found:?}; expected \"1.0\", \"1.1\", or \"1.3\"")]
    UnsupportedSchema {
        /// Unsupported revision read from the manifest.
        found: String,
    },

    /// A known task field or directory shape is invalid.
    #[error("task configuration {path} is invalid: {message}")]
    Invalid {
        /// File or directory containing the invalid value.
        path: PathBuf,
        /// Validation failure.
        message: String,
    },

    /// A required task file or directory is absent.
    #[error("task is missing required file {path}")]
    MissingFile {
        /// Missing path.
        path: PathBuf,
    },

    /// The immutable task package could not be fingerprinted.
    #[error("failed to fingerprint task package {path}: {source}")]
    Fingerprint {
        /// Task root that could not be fingerprinted.
        path: PathBuf,
        /// Filesystem or package-entry failure.
        #[source]
        source: std::io::Error,
    },

    /// The task package changed after it was loaded.
    #[error(
        "task package changed after load at {path}: expected digest {expected}, found {actual}"
    )]
    ContentChanged {
        /// Canonical task root.
        path: PathBuf,
        /// Digest captured when the task was loaded.
        expected: String,
        /// Digest observed immediately before use.
        actual: String,
    },
}

impl Task {
    /// Loads the Terminal-Bench 2.1 task rooted at `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLoadError`] when the directory cannot be resolved, a
    /// required task file is absent or unreadable, the TOML is malformed, or
    /// the declared Terminal-Bench 2.1 fields are invalid.
    pub fn load(directory: impl AsRef<Path>) -> Result<Self, TaskLoadError> {
        let requested = directory.as_ref();
        let root =
            fs::canonicalize(requested).map_err(|source| TaskLoadError::ResolveDirectory {
                path: requested.to_path_buf(),
                source,
            })?;
        if !root.is_dir() {
            return Err(TaskLoadError::Invalid {
                path: root,
                message: "task root is not a directory".to_owned(),
            });
        }

        let package = TaskPackage::load(&root).map_err(|source| TaskLoadError::Fingerprint {
            path: root.clone(),
            source,
        })?;
        let config_path = root.join(TASK_CONFIG);
        let config_text = read_package_file(&package, &root, TASK_CONFIG)?;
        let raw: RawTask = toml::from_str(&config_text).map_err(|source| TaskLoadError::Parse {
            path: config_path.clone(),
            source,
        })?;
        let schema_version = match (&raw.schema_version, &raw.version) {
            (Some(schema), None) | (None, Some(schema)) => schema.as_str(),
            (Some(schema), Some(version)) if schema == version => schema.as_str(),
            (Some(schema), Some(version)) => {
                return Err(TaskLoadError::Invalid {
                    path: config_path,
                    message: format!(
                        "schema_version {schema:?} conflicts with version {version:?}"
                    ),
                });
            }
            (None, None) => {
                return Err(TaskLoadError::UnsupportedSchema {
                    found: "<missing>".to_owned(),
                });
            }
        };
        if !matches!(schema_version, "1.0" | "1.1" | "1.3") {
            return Err(TaskLoadError::UnsupportedSchema {
                found: schema_version.to_owned(),
            });
        }
        let legacy_schema = schema_version == "1.0";

        let instruction_path = root.join(TASK_INSTRUCTION);
        let prompt = strip_leading_canary(&read_package_file(&package, &root, TASK_INSTRUCTION)?);
        if prompt.trim().is_empty() {
            return Err(TaskLoadError::Invalid {
                path: instruction_path,
                message: "instruction is empty".to_owned(),
            });
        }
        let transcript_path = root.join(TASK_TRANSCRIPT);
        let transcript = package
            .read_file(Path::new(TASK_TRANSCRIPT))
            .map_err(|source| TaskLoadError::Read {
                path: transcript_path.clone(),
                source,
            })?
            .map(|bytes| {
                serde_json::from_slice::<TaskTranscript>(&bytes).map_err(|source| {
                    TaskLoadError::TranscriptParse {
                        path: transcript_path.clone(),
                        source,
                    }
                })
            })
            .transpose()?
            .map_or_else(Vec::new, |transcript| transcript.messages);
        Prompt::new(prompt.clone())
            .with_transcript(transcript.clone())
            .validate()
            .map_err(|error| TaskLoadError::Invalid {
                path: transcript_path,
                message: error.to_string(),
            })?;

        let verifier_script = root.join(VERIFIER_SCRIPT);
        require_file(&verifier_script)?;
        let environment_directory = root.join(TASK_ENVIRONMENT);
        if !package.contains_directory(Path::new(TASK_ENVIRONMENT)) {
            return Err(TaskLoadError::MissingFile {
                path: environment_directory,
            });
        }

        let (network, verifier_network) = raw.network_policies(&config_path)?;
        let task_info = match raw.task {
            Some(task) => task,
            None if legacy_schema => RawTaskInfo {
                name: root
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .ok_or_else(|| TaskLoadError::Invalid {
                        path: config_path.clone(),
                        message: "version 1.0 task directory name must be valid UTF-8".to_owned(),
                    })?
                    .to_owned(),
                description: String::new(),
                benchmark_prompt_chars: None,
                benchmark_case_type: None,
            },
            None => {
                return Err(TaskLoadError::Invalid {
                    path: config_path,
                    message: "task table is required by this schema version".to_owned(),
                });
            }
        };
        let name = required_string(&config_path, "task.name", task_info.name)?;
        if task_info.benchmark_prompt_chars == Some(0) {
            return Err(TaskLoadError::Invalid {
                path: config_path,
                message: "task.benchmark_prompt_chars must be positive when present".to_owned(),
            });
        }
        let benchmark_case_type = task_info
            .benchmark_case_type
            .map(|value| required_string(&config_path, "task.benchmark_case_type", value))
            .transpose()?
            .map(String::into_boxed_str);
        let image = raw
            .environment
            .docker_image
            .map(|image| required_string(&config_path, "environment.docker_image", image))
            .transpose()?
            .unwrap_or_else(|| "local-dockerfile".to_owned());
        let content_digest = package.digest().to_owned().into_boxed_str();
        let task = Self {
            root,
            content_digest,
            package: Arc::new(package),
            dataset: None,
            dataset_revision: None,
            name: name.into_boxed_str(),
            description: task_info.description.into_boxed_str(),
            prompt_chars: u64::try_from(
                transcript
                    .iter()
                    .map(|message| message.content().chars().count())
                    .sum::<usize>()
                    .saturating_add(prompt.chars().count()),
            )
            .unwrap_or(u64::MAX),
            benchmark_prompt_chars: task_info.benchmark_prompt_chars,
            benchmark_case_type,
            prompt: prompt.into_boxed_str(),
            transcript,
            agent_instructions: raw
                .agent
                .instructions
                .map(|instructions| {
                    required_string(&config_path, "agent.instructions", instructions)
                        .map(String::into_boxed_str)
                })
                .transpose()?,
            image: OciImage {
                reference: image.into_boxed_str(),
            },
            agent_timeout: duration(&config_path, "agent.timeout_sec", raw.agent.timeout_sec)?,
            verifier: Verifier {
                script: verifier_script,
                timeout: duration(
                    &config_path,
                    "verifier.timeout_sec",
                    raw.verifier.timeout_sec,
                )?,
                environment: raw.verifier.env,
                environment_mode: raw.verifier.environment_mode,
                collect: raw.verifier.collect,
                scoring_policy: raw.verifier.scoring_policy,
                network: verifier_network,
            },
            artifacts: raw
                .artifacts
                .into_iter()
                .map(|artifact| artifact.validate(&config_path))
                .collect::<Result<_, _>>()?,
            output: raw.output,
            resources: Resources {
                cpus: positive(&config_path, "environment.cpus", raw.environment.cpus)?,
                memory_mb: positive(
                    &config_path,
                    "environment.memory_mb",
                    raw.environment.memory_mb,
                )?,
                storage_mb: positive(
                    &config_path,
                    "environment.storage_mb",
                    raw.environment.storage_mb,
                )?,
                gpus: raw.environment.gpus,
            },
            network,
            environment: raw.environment.env,
            requires_compose: raw.environment.custom_docker_compose
                || (raw.metadata.custom_docker_compose
                    && !raw.metadata.moved_workdir_from_compose_to_dockerfile),
        };
        task.validate_package()?;
        Ok(task)
    }

    /// Returns the canonical task root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the stable content digest of the complete task package.
    ///
    /// Durable profile ledgers use this identity to prevent a task selector
    /// from silently changing after desired coordinates are materialized.
    #[must_use]
    pub fn package_digest(&self) -> &str {
        &self.content_digest
    }

    pub(crate) fn content_digest(&self) -> &str {
        self.package_digest()
    }

    /// Re-fingerprints every packaged execution input and rejects mutation
    /// since this task was loaded.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLoadError::Fingerprint`] when the package cannot be read,
    /// or [`TaskLoadError::ContentChanged`] when its canonical digest changed.
    pub fn validate_package(&self) -> Result<(), TaskLoadError> {
        let started = Instant::now();
        let package = self.current_package();
        let duration_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        match &package {
            Ok(package) => tracing::info!(
                target: "nanocodex_eval",
                task_name = self.name(),
                task_package_entries = package.entry_count(),
                task_package_file_bytes = package.file_bytes(),
                duration_ns,
                status = "unchanged",
                "validated task package identity"
            ),
            Err(error) => tracing::info!(
                target: "nanocodex_eval",
                task_name = self.name(),
                duration_ns,
                status = "changed",
                error = %error,
                "task package validation failed"
            ),
        }
        package.map(drop)
    }

    /// Materializes the environment tree captured when this task was loaded.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLoadError`] when a captured file changed, materialization
    /// failed, or the package mutated during the copy.
    #[doc(hidden)]
    pub fn materialize_environment(&self, destination: &Path) -> Result<(), TaskLoadError> {
        self.materialize_package_directory(Path::new(TASK_ENVIRONMENT), destination)?;
        let dockerfile = destination.join("Dockerfile");
        if !dockerfile.exists() && self.image.reference() != "local-dockerfile" {
            fs::write(&dockerfile, format!("FROM {}\n", self.image.reference())).map_err(
                |source| TaskLoadError::Fingerprint {
                    path: dockerfile,
                    source,
                },
            )?;
        }
        Ok(())
    }

    /// Materializes the verifier tree captured when this task was loaded.
    ///
    /// Files, directories, and Unix modes are reproduced from the same
    /// manifest used for task identity. The source package is revalidated
    /// after materialization.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLoadError`] when a captured file changed, materialization
    /// failed, or the package mutated during the copy.
    #[doc(hidden)]
    pub fn materialize_verifier_files(&self, destination: &Path) -> Result<(), TaskLoadError> {
        self.materialize_package_directory(Path::new("tests"), destination)
    }

    pub(crate) fn materialize_package(&self, destination: &Path) -> Result<(), TaskLoadError> {
        self.package
            .materialize(destination)
            .map_err(|source| TaskLoadError::Fingerprint {
                path: self.root.clone(),
                source,
            })?;
        self.validate_package()
    }

    /// Reads the verifier script captured when this task was loaded.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLoadError`] when the captured script changed, became
    /// unreadable, or the package mutated during the read.
    #[doc(hidden)]
    pub fn verifier_script_bytes(&self) -> Result<Vec<u8>, TaskLoadError> {
        let script = self
            .package
            .read_file(Path::new(VERIFIER_SCRIPT))
            .map_err(|source| TaskLoadError::Read {
                path: self.verifier.script.clone(),
                source,
            })?
            .ok_or_else(|| TaskLoadError::MissingFile {
                path: self.verifier.script.clone(),
            })?;
        self.validate_package()?;
        Ok(script)
    }

    /// Returns the stable task name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the evaluator dataset identity attached by a prepared profile.
    #[must_use]
    pub fn dataset(&self) -> Option<&str> {
        self.dataset.as_deref()
    }

    /// Returns the pinned upstream revision attached by an imported dataset.
    #[must_use]
    pub fn dataset_revision(&self) -> Option<&str> {
        self.dataset_revision.as_deref()
    }

    pub(crate) fn attach_dataset(mut self, dataset: &str, revision: Option<&str>) -> Self {
        self.dataset = Some(dataset.to_owned().into_boxed_str());
        self.dataset_revision = revision.map(|revision| revision.to_owned().into_boxed_str());
        self
    }

    /// Returns the human-readable task description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the complete instruction presented to the agent.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the synthetic conversation preceding the final user prompt.
    #[must_use]
    pub fn transcript(&self) -> &[PromptMessage] {
        &self.transcript
    }

    /// Builds the complete typed prompt accepted by the native agent.
    #[must_use]
    pub fn agent_prompt(&self) -> Prompt {
        Prompt::new(self.prompt.to_string()).with_transcript(self.transcript.clone())
    }

    /// Returns the number of Unicode scalar values across the complete
    /// transcript and final user prompt.
    #[must_use]
    pub const fn prompt_chars(&self) -> u64 {
        self.prompt_chars
    }

    /// Returns the source benchmark's declared prompt-size dimension, when present.
    #[must_use]
    pub const fn benchmark_prompt_chars(&self) -> Option<u64> {
        self.benchmark_prompt_chars
    }

    /// Returns the source benchmark's case-type dimension, when present.
    #[must_use]
    pub fn benchmark_case_type(&self) -> Option<&str> {
        self.benchmark_case_type.as_deref()
    }

    /// Benchmark-owned model instructions applied independently of the user prompt.
    #[must_use]
    pub fn agent_instructions(&self) -> Option<&str> {
        self.agent_instructions.as_deref()
    }

    /// Files copied into the disposable native workspace before an attempt.
    #[must_use]
    pub fn environment_directory(&self) -> PathBuf {
        self.root.join(TASK_ENVIRONMENT)
    }

    /// Returns the task's declared OCI image.
    #[must_use]
    pub const fn image(&self) -> &OciImage {
        &self.image
    }

    /// Returns the maximum agent execution duration.
    #[must_use]
    pub const fn agent_timeout(&self) -> Duration {
        self.agent_timeout
    }

    /// Returns the verifier recipe.
    #[must_use]
    pub const fn verifier(&self) -> &Verifier {
        &self.verifier
    }

    /// Returns task-relative artifact paths requested after verification.
    #[must_use]
    pub fn artifacts(&self) -> &[TaskArtifact] {
        &self.artifacts
    }

    /// Reads the optional benchmark-owned artifact capture phase.
    ///
    /// The script runs in the candidate VM after agent tools have stopped and
    /// before declared artifacts are copied into a separate verifier VM.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLoadError`] when the captured script changed or became
    /// unreadable after the task was loaded.
    #[doc(hidden)]
    pub fn pre_artifacts_script_bytes(&self) -> Result<Option<Vec<u8>>, TaskLoadError> {
        self.package
            .read_file(Path::new(PRE_ARTIFACTS_SCRIPT))
            .map_err(|source| TaskLoadError::Read {
                path: self.root.join(PRE_ARTIFACTS_SCRIPT),
                source,
            })
    }

    /// Returns the candidate value consumed by the verifier.
    #[must_use]
    pub const fn output(&self) -> TaskOutput {
        self.output
    }

    /// Returns declared resource requirements.
    #[must_use]
    pub const fn resources(&self) -> &Resources {
        &self.resources
    }

    /// Returns the task's network policy.
    #[must_use]
    pub const fn network(&self) -> NetworkPolicy {
        self.network
    }

    /// Returns environment variables supplied to the task process.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// Returns whether the task requires a custom Docker Compose topology.
    #[must_use]
    pub const fn requires_compose(&self) -> bool {
        self.requires_compose
    }

    fn current_package(&self) -> Result<TaskPackage, TaskLoadError> {
        let package =
            TaskPackage::load(&self.root).map_err(|source| TaskLoadError::Fingerprint {
                path: self.root.clone(),
                source,
            })?;
        if package.digest() != self.content_digest() {
            return Err(TaskLoadError::ContentChanged {
                path: self.root.clone(),
                expected: self.content_digest().to_owned(),
                actual: package.digest().to_owned(),
            });
        }
        Ok(package)
    }

    fn materialize_package_directory(
        &self,
        package_directory: &Path,
        destination: &Path,
    ) -> Result<(), TaskLoadError> {
        self.package
            .materialize_directory(package_directory, destination)
            .map_err(|source| TaskLoadError::Fingerprint {
                path: self.root.clone(),
                source,
            })?;
        self.validate_package()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskTranscript {
    messages: Vec<PromptMessage>,
}

impl OciImage {
    /// Returns the manifest's image reference.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }
}

impl NetworkPolicy {
    /// Returns the stable artifact and telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Disabled => "no-network",
        }
    }
}

impl Verifier {
    /// Returns the canonical verifier script path.
    #[must_use]
    pub fn script(&self) -> &Path {
        &self.script
    }

    /// Returns the verifier execution deadline.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Returns environment variables supplied to the verifier.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// Returns where verification executes.
    #[must_use]
    pub const fn environment_mode(&self) -> VerifierEnvironmentMode {
        self.environment_mode
    }

    /// Returns post-verifier collection commands.
    #[must_use]
    pub fn collect(&self) -> &[VerifierCollect] {
        &self.collect
    }

    /// Returns the benchmark-owned binary classification policy.
    #[must_use]
    pub const fn scoring_policy(&self) -> ScoringPolicy {
        self.scoring_policy
    }

    /// Returns the verifier VM's network policy.
    #[must_use]
    pub const fn network(&self) -> NetworkPolicy {
        self.network
    }
}

impl ScoringPolicy {
    /// Classifies a non-empty set of finite verifier rewards.
    #[must_use]
    pub fn passes(self, rewards: &BTreeMap<String, f64>) -> bool {
        match self {
            Self::AllRewardsPositive => rewards.values().all(|reward| *reward > 0.0),
            Self::AllRewardsOne => rewards
                .values()
                .all(|reward| reward.to_bits() == 1.0_f64.to_bits()),
        }
    }

    /// Returns the stable retained-evidence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllRewardsPositive => "all_rewards_positive-v1",
            Self::AllRewardsOne => "all_rewards_one-v1",
        }
    }
}

impl VerifierCollect {
    /// Returns the complete shell command.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }
}

impl TaskArtifact {
    /// Returns the absolute source path inside the guest.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Returns source-relative paths omitted while archiving this artifact.
    #[must_use]
    pub fn exclude(&self) -> &[PathBuf] {
        &self.exclude
    }

    /// Returns the Compose service that owns the artifact, when declared.
    #[must_use]
    pub fn service(&self) -> Option<&str> {
        self.service.as_deref()
    }
}

impl VerifierEnvironmentMode {
    /// Returns the stable manifest and artifact spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Separate => "separate",
        }
    }
}

#[derive(Deserialize)]
struct RawTask {
    #[serde(default)]
    schema_version: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    artifacts: Vec<RawTaskArtifact>,
    #[serde(default)]
    output: TaskOutput,
    #[serde(default)]
    task: Option<RawTaskInfo>,
    #[serde(default)]
    metadata: RawMetadata,
    agent: RawPhase,
    verifier: RawVerifier,
    environment: RawEnvironment,
}

impl RawTask {
    fn network_policies(
        &self,
        config: &Path,
    ) -> Result<(NetworkPolicy, NetworkPolicy), TaskLoadError> {
        let environment = self.environment.network_policy(config)?;
        let agent = self.agent.network_mode.unwrap_or(environment);
        let verifier = self.verifier.network_mode.unwrap_or(environment);
        if agent != verifier && self.verifier.environment_mode != VerifierEnvironmentMode::Separate
        {
            return Err(TaskLoadError::Invalid {
                path: config.to_path_buf(),
                message: "distinct agent and verifier network modes require a separate verifier environment"
                    .to_owned(),
            });
        }
        Ok((agent.policy(config)?, verifier.policy(config)?))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawTaskArtifact {
    Path(PathBuf),
    Detailed {
        source: PathBuf,
        #[serde(default)]
        exclude: Vec<PathBuf>,
        #[serde(default)]
        service: Option<String>,
    },
}

impl RawTaskArtifact {
    fn validate(self, config: &Path) -> Result<TaskArtifact, TaskLoadError> {
        let (source, exclude, service) = match self {
            Self::Path(source) => (source, Vec::new(), None),
            Self::Detailed {
                source,
                exclude,
                service,
            } => (source, exclude, service),
        };
        if !safe_absolute_guest_path(&source) {
            return Err(TaskLoadError::Invalid {
                path: config.to_path_buf(),
                message: format!(
                    "artifact source must be a safe absolute guest path: {}",
                    source.display()
                ),
            });
        }
        for path in &exclude {
            if path.as_os_str().is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(TaskLoadError::Invalid {
                    path: config.to_path_buf(),
                    message: format!(
                        "artifact exclusion must be a safe source-relative path: {}",
                        path.display()
                    ),
                });
            }
        }
        let service = service
            .map(|service| required_string(config, "artifacts.service", service))
            .transpose()?
            .map(String::into_boxed_str);
        Ok(TaskArtifact {
            source,
            exclude,
            service,
        })
    }
}

fn safe_absolute_guest_path(path: &Path) -> bool {
    path.is_absolute()
        && path.strip_prefix("/").is_ok_and(|relative| {
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
}

#[derive(Default, Deserialize)]
struct RawMetadata {
    #[serde(default)]
    custom_docker_compose: bool,
    #[serde(default)]
    moved_workdir_from_compose_to_dockerfile: bool,
}

#[derive(Deserialize)]
struct RawTaskInfo {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    benchmark_prompt_chars: Option<u64>,
    #[serde(default)]
    benchmark_case_type: Option<String>,
}

#[derive(Deserialize)]
struct RawPhase {
    timeout_sec: f64,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    network_mode: Option<RawNetworkMode>,
}

#[derive(Deserialize)]
struct RawVerifier {
    timeout_sec: f64,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    environment_mode: VerifierEnvironmentMode,
    #[serde(default)]
    collect: Vec<VerifierCollect>,
    #[serde(default)]
    network_mode: Option<RawNetworkMode>,
    #[serde(default)]
    scoring_policy: ScoringPolicy,
}

#[derive(Deserialize)]
struct RawEnvironment {
    #[serde(default)]
    docker_image: Option<String>,
    cpus: u32,
    memory_mb: u64,
    storage_mb: u64,
    #[serde(default)]
    gpus: u32,
    #[serde(default = "enabled")]
    allow_internet: bool,
    #[serde(default)]
    network_mode: Option<RawNetworkMode>,
    #[serde(default)]
    custom_docker_compose: bool,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

impl RawEnvironment {
    fn network_policy(&self, config: &Path) -> Result<RawNetworkMode, TaskLoadError> {
        let compatibility = if self.allow_internet {
            RawNetworkMode::Public
        } else {
            RawNetworkMode::NoNetwork
        };
        let policy = self.network_mode.unwrap_or(compatibility);
        policy.policy(config)?;
        Ok(policy)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawNetworkMode {
    Public,
    NoNetwork,
    Allowlist,
}

impl RawNetworkMode {
    fn policy(self, config: &Path) -> Result<NetworkPolicy, TaskLoadError> {
        match self {
            Self::Public => Ok(NetworkPolicy::Public),
            Self::NoNetwork => Ok(NetworkPolicy::Disabled),
            Self::Allowlist => Err(TaskLoadError::Invalid {
                path: config.to_path_buf(),
                message:
                    "allowlist network mode is not implemented; refusing to widen it to public"
                        .to_owned(),
            }),
        }
    }
}

impl<'de> Deserialize<'de> for VerifierEnvironmentMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "same" => Ok(Self::Same),
            "separate" => Ok(Self::Separate),
            mode => Err(serde::de::Error::unknown_variant(
                mode,
                &["same", "separate"],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for VerifierCollect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawCollect {
            command: String,
        }

        let raw = RawCollect::deserialize(deserializer)?;
        if raw.command.trim().is_empty() {
            return Err(serde::de::Error::custom(
                "verifier collect command must not be empty",
            ));
        }
        Ok(Self {
            command: raw.command.into_boxed_str(),
        })
    }
}

const fn enabled() -> bool {
    true
}

fn read_package_file(
    package: &TaskPackage,
    root: &Path,
    relative: &str,
) -> Result<String, TaskLoadError> {
    let path = root.join(relative);
    let bytes = package
        .read_file(Path::new(relative))
        .map_err(|source| TaskLoadError::Read {
            path: path.clone(),
            source,
        })?
        .ok_or_else(|| TaskLoadError::MissingFile { path: path.clone() })?;
    String::from_utf8(bytes).map_err(|source| TaskLoadError::Read {
        path,
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
    })
}

fn require_file(path: &Path) -> Result<(), TaskLoadError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(TaskLoadError::MissingFile {
            path: path.to_path_buf(),
        })
    }
}

fn required_string(path: &Path, field: &str, value: String) -> Result<String, TaskLoadError> {
    if value.trim().is_empty() {
        Err(TaskLoadError::Invalid {
            path: path.to_path_buf(),
            message: format!("{field} must not be empty"),
        })
    } else {
        Ok(value)
    }
}

fn duration(path: &Path, field: &str, seconds: f64) -> Result<Duration, TaskLoadError> {
    if seconds <= 0.0 {
        return Err(TaskLoadError::Invalid {
            path: path.to_path_buf(),
            message: format!("{field} must be greater than zero"),
        });
    }
    Duration::try_from_secs_f64(seconds).map_err(|error| TaskLoadError::Invalid {
        path: path.to_path_buf(),
        message: format!("{field} is invalid: {error}"),
    })
}

fn positive<T>(path: &Path, field: &str, value: T) -> Result<T, TaskLoadError>
where
    T: Copy + Default + PartialEq,
{
    if value == T::default() {
        Err(TaskLoadError::Invalid {
            path: path.to_path_buf(),
            message: format!("{field} must be greater than zero"),
        })
    } else {
        Ok(value)
    }
}

fn strip_leading_canary(text: &str) -> String {
    let mut lines = text.lines().peekable();
    while lines.peek().is_some_and(|line| is_canary(line)) {
        lines.next();
    }
    while lines.peek().is_some_and(|line| line.trim().is_empty()) {
        lines.next();
    }
    lines.collect::<Vec<_>>().join("\n")
}

fn is_canary(line: &str) -> bool {
    let line = line.trim();
    let comment = line.starts_with('#') || (line.starts_with("<!--") && line.ends_with("-->"));
    comment && line.to_ascii_lowercase().contains("canary")
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{NetworkPolicy, ScoringPolicy, Task, VerifierEnvironmentMode};

    #[test]
    fn loads_terminal_bench_2_1_task_directory() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("tests")).unwrap();
        fs::create_dir(directory.path().join("environment")).unwrap();
        fs::write(
            directory.path().join("task.toml"),
            r#"
schema_version = "1.1"

[task]
name = "terminal-bench/example"
description = "Example task"
benchmark_prompt_chars = 12
benchmark_case_type = "fixture"

[metadata]
custom_docker_compose = true

[agent]
timeout_sec = 900.0

[verifier]
timeout_sec = 600.0

[verifier.env]
ANSWER = "42"

[environment]
docker_image = "example/task:20251031"
cpus = 2
memory_mb = 4096
storage_mb = 10240
gpus = 0
allow_internet = false

[environment.env]
MODE = "test"
"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("instruction.md"),
            "# terminal-bench-canary secret\n\nFix the task.\n",
        )
        .unwrap();
        fs::write(directory.path().join("tests/test.sh"), "#!/bin/sh\n").unwrap();

        let task = Task::load(directory.path()).unwrap();

        assert_eq!(task.name(), "terminal-bench/example");
        assert_eq!(task.prompt(), "Fix the task.");
        assert_eq!(task.prompt_chars(), 13);
        assert_eq!(task.benchmark_prompt_chars(), Some(12));
        assert_eq!(task.benchmark_case_type(), Some("fixture"));
        assert_eq!(task.image().reference(), "example/task:20251031");
        assert_eq!(task.resources().cpus, 2);
        assert_eq!(task.network(), NetworkPolicy::Disabled);
        assert_eq!(task.environment()["MODE"], "test");
        assert_eq!(task.verifier().environment()["ANSWER"], "42");
        assert_eq!(
            task.verifier().scoring_policy(),
            ScoringPolicy::AllRewardsPositive
        );
        assert!(task.requires_compose());
        let materialized = tempdir().unwrap();
        task.materialize_environment(materialized.path()).unwrap();
        assert_eq!(
            fs::read_to_string(materialized.path().join("Dockerfile")).unwrap(),
            "FROM example/task:20251031\n"
        );
    }

    #[test]
    fn loads_harbor_1_0_task_name_from_its_directory() {
        let parent = tempdir().unwrap();
        let directory = parent.path().join("frontier-task");
        fs::create_dir(&directory).unwrap();
        fs::create_dir(directory.join("tests")).unwrap();
        fs::create_dir(directory.join("environment")).unwrap();
        fs::write(
            directory.join("task.toml"),
            r#"
version = "1.0"

[agent]
timeout_sec = 72000.0

[verifier]
timeout_sec = 86400.0

[environment]
docker_image = "example/frontier-task:v1"
cpus = 8
memory_mb = 32768
storage_mb = 51200
gpus = 0
allow_internet = false
"#,
        )
        .unwrap();
        fs::write(directory.join("instruction.md"), "Optimize it.").unwrap();
        fs::write(directory.join("tests/test.sh"), "#!/bin/sh\n").unwrap();

        let task = Task::load(&directory).unwrap();

        assert_eq!(task.name(), "frontier-task");
        assert_eq!(task.agent_timeout(), std::time::Duration::from_secs(72_000));
        assert_eq!(
            task.verifier().timeout(),
            std::time::Duration::from_secs(86_400)
        );
    }

    #[test]
    fn loads_and_fingerprints_a_synthetic_transcript() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("tests")).unwrap();
        fs::create_dir(directory.path().join("environment")).unwrap();
        fs::write(
            directory.path().join("task.toml"),
            r#"schema_version = "1.3"
[task]
name = "mrcr/example"
[agent]
timeout_sec = 10.0
[verifier]
timeout_sec = 10.0
[environment]
docker_image = "debian:bookworm-slim"
cpus = 1
memory_mb = 1024
storage_mb = 1024
"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("instruction.md"),
            "Return the second answer.",
        )
        .unwrap();
        fs::write(
            directory.path().join("transcript.json"),
            r#"{"messages":[{"role":"user","content":"first"},{"role":"assistant","content":"one"},{"role":"user","content":"second"},{"role":"assistant","content":"two"}]}"#,
        )
        .unwrap();
        fs::write(directory.path().join("tests/test.sh"), "#!/bin/sh\n").unwrap();

        let task = Task::load(directory.path()).unwrap();

        assert_eq!(task.transcript().len(), 4);
        assert_eq!(task.agent_prompt().transcript(), task.transcript());
        assert_eq!(task.prompt_chars(), 42);
        let digest = task.content_digest().to_owned();
        fs::write(
            directory.path().join("transcript.json"),
            r#"{"messages":[{"role":"user","content":"first"},{"role":"assistant","content":"changed"}]}"#,
        )
        .unwrap();
        assert_ne!(
            Task::load(directory.path()).unwrap().content_digest(),
            digest
        );
    }

    #[test]
    fn scoring_policies_distinguish_partial_from_exact_continuous_rewards() {
        let partial = std::collections::BTreeMap::from([("f1".to_owned(), 2.0 / 3.0)]);
        let exact = std::collections::BTreeMap::from([("f1".to_owned(), 1.0)]);

        assert!(ScoringPolicy::AllRewardsPositive.passes(&partial));
        assert!(!ScoringPolicy::AllRewardsOne.passes(&partial));
        assert!(ScoringPolicy::AllRewardsOne.passes(&exact));
        assert_eq!(ScoringPolicy::AllRewardsOne.as_str(), "all_rewards_one-v1");
    }

    #[test]
    fn loads_migrated_compose_task_as_a_single_image() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("tests")).unwrap();
        fs::create_dir(directory.path().join("environment")).unwrap();
        fs::write(
            directory.path().join("task.toml"),
            r#"
schema_version = "1.1"

[task]
name = "terminal-bench/migrated-compose"

[metadata]
custom_docker_compose = true
moved_workdir_from_compose_to_dockerfile = true

[agent]
timeout_sec = 900.0

[verifier]
timeout_sec = 600.0

[environment]
docker_image = "example/task:20251031"
cpus = 2
memory_mb = 4096
storage_mb = 10240
"#,
        )
        .unwrap();
        fs::write(directory.path().join("instruction.md"), "Fix the task.").unwrap();
        fs::write(directory.path().join("tests/test.sh"), "#!/bin/sh\n").unwrap();

        let task = Task::load(directory.path()).unwrap();

        assert!(!task.requires_compose());
    }

    #[test]
    fn rejects_missing_verifier_script() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("environment")).unwrap();
        fs::write(
            directory.path().join("task.toml"),
            r#"
schema_version = "1.1"
[task]
name = "terminal-bench/example"
[agent]
timeout_sec = 1.0
[verifier]
timeout_sec = 1.0
[environment]
docker_image = "example/task:latest"
cpus = 1
memory_mb = 1
storage_mb = 1
"#,
        )
        .unwrap();
        fs::write(directory.path().join("instruction.md"), "Do it.").unwrap();

        let error = Task::load(directory.path()).unwrap_err();
        assert!(error.to_string().contains("tests/test.sh"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_execution_inputs() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("tests")).unwrap();
        fs::create_dir(directory.path().join("environment")).unwrap();
        fs::write(
            directory.path().join("task.toml"),
            r#"
schema_version = "1.1"
[task]
name = "terminal-bench/symlink"
[agent]
timeout_sec = 1.0
[verifier]
timeout_sec = 1.0
[environment]
docker_image = "example/task:latest"
cpus = 1
memory_mb = 1
storage_mb = 1
"#,
        )
        .unwrap();
        fs::write(directory.path().join("instruction.md"), "Do it.").unwrap();
        fs::write(directory.path().join("tests/test.sh"), "exit 0\n").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", directory.path().join("environment/escape"))
            .unwrap();

        let error = Task::load(directory.path()).unwrap_err();

        assert!(matches!(error, super::TaskLoadError::Fingerprint { .. }));
        assert!(
            error
                .to_string()
                .contains("symlink target must be relative")
        );
        assert!(error.to_string().contains("/etc/passwd"));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_relative_symlinks_within_execution_inputs() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("tests")).unwrap();
        fs::create_dir_all(directory.path().join("environment/docs")).unwrap();
        fs::write(
            directory.path().join("task.toml"),
            r#"
schema_version = "1.1"
[task]
name = "terminal-bench/symlink"
[agent]
timeout_sec = 1.0
[verifier]
timeout_sec = 1.0
[environment]
docker_image = "example/task:latest"
cpus = 1
memory_mb = 1
storage_mb = 1
"#,
        )
        .unwrap();
        fs::write(directory.path().join("instruction.md"), "Do it.").unwrap();
        fs::write(directory.path().join("tests/test.sh"), "exit 0\n").unwrap();
        fs::write(
            directory.path().join("environment/docs/target"),
            "contents\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("docs/target", directory.path().join("environment/link"))
            .unwrap();

        let task = Task::load(directory.path()).unwrap();
        let materialized = tempdir().unwrap();
        task.materialize_environment(materialized.path()).unwrap();

        assert_eq!(
            fs::read_link(materialized.path().join("link")).unwrap(),
            PathBuf::from("docs/target")
        );
        assert_eq!(
            fs::read_to_string(materialized.path().join("link")).unwrap(),
            "contents\n"
        );
    }

    #[test]
    fn loads_frontier_bench_task_with_a_separate_verifier() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("tests")).unwrap();
        fs::create_dir(directory.path().join("environment")).unwrap();
        fs::write(
            directory.path().join("task.toml"),
            r#"
schema_version = "1.1"
artifacts = ["/app/output.txt"]

[task]
name = "terminal-bench/frontier-example"

[agent]
timeout_sec = 900.0
network_mode = "no-network"

[verifier]
timeout_sec = 600.0
environment_mode = "separate"
network_mode = "public"

[[verifier.collect]]
command = "cp /app/output.txt /tmp/output.txt"

[environment]
cpus = 2
memory_mb = 4096
storage_mb = 10240
"#,
        )
        .unwrap();
        fs::write(directory.path().join("instruction.md"), "Fix the task.").unwrap();
        fs::write(
            directory.path().join("environment/Dockerfile"),
            "FROM scratch\n",
        )
        .unwrap();
        fs::write(directory.path().join("tests/Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(directory.path().join("tests/test.sh"), "#!/bin/sh\n").unwrap();
        fs::write(
            directory.path().join("pre_artifacts.sh"),
            "#!/bin/sh\ncp /app/output.txt /tmp/output.txt\n",
        )
        .unwrap();

        let task = Task::load(directory.path()).unwrap();

        assert_eq!(task.image().reference(), "local-dockerfile");
        assert_eq!(task.network(), NetworkPolicy::Disabled);
        assert_eq!(task.verifier().network(), NetworkPolicy::Public);
        assert_eq!(
            task.verifier().environment_mode(),
            VerifierEnvironmentMode::Separate
        );
        assert_eq!(
            task.artifacts()[0].source(),
            PathBuf::from("/app/output.txt")
        );
        assert_eq!(
            task.verifier().collect()[0].command(),
            "cp /app/output.txt /tmp/output.txt"
        );
        assert_eq!(
            task.pre_artifacts_script_bytes().unwrap().unwrap(),
            b"#!/bin/sh\ncp /app/output.txt /tmp/output.txt\n"
        );
    }

    #[test]
    fn loads_the_native_suite_fixtures() {
        let tasks = ["write-greeting", "uppercase-message", "extract-todos"];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tasks");

        for name in tasks {
            let task = Task::load(root.join(name)).unwrap();
            assert_eq!(task.name(), format!("nanoeval/{name}"));
            assert!(!task.prompt().is_empty());
            assert!(!task.requires_compose());
        }
    }
}
