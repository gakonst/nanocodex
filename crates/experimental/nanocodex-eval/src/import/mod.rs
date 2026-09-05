//! Third-party dataset conversion into immutable evaluator tasks.
//!
//! Import is a build step. Format-specific readers produce a
//! [`DatasetPlan`](crate::import::DatasetPlan), and
//! [`ImportStore`](crate::import::ImportStore) snapshots every execution input
//! into a content-addressed
//! dataset. The normal evaluator then sees only [`Task`] values; VM image
//! caching, writable overlays, scheduling, resume, and evidence retention do
//! not branch on the upstream benchmark.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{self, Read as _, Write as _},
    os::unix::fs::PermissionsExt as _,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use nanocodex_oai_api::PromptMessage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Resources, ScoringPolicy, Task, TaskLoadError, TaskOutput};

const IMPORT_SCHEMA: &str = "nanocodex-import-v1";
const PLAN_INDEX_SCHEMA: &str = "nanocodex-import-plan-index-v1";
const MANIFEST_FILE: &str = "dataset.json";

/// A format-specific reader that describes a deterministic dataset import.
pub trait DatasetImporter {
    /// Produces the complete import plan without running an agent or grader.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when upstream metadata is malformed, missing,
    /// or cannot be represented without changing benchmark semantics.
    fn plan(&self) -> Result<DatasetPlan, ImportError>;
}

/// Durable content-addressed storage for normalized datasets.
#[derive(Clone, Debug)]
pub struct ImportStore {
    root: PathBuf,
}

/// One immutable imported dataset and its evaluator-ready tasks.
#[derive(Clone, Debug)]
pub struct ImportedDataset {
    root: PathBuf,
    digest: Box<str>,
    source: SourceIdentity,
    tasks: Vec<Task>,
}

/// A complete, deterministic dataset conversion plan.
#[derive(Debug)]
pub struct DatasetPlan {
    name: Box<str>,
    source: SourceIdentity,
    cases: Vec<CasePlan>,
}

/// Safe provenance retained for an imported upstream source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceIdentity {
    kind: Box<str>,
    revision: Box<str>,
    digest: Box<str>,
}

/// One normalized case in a [`DatasetPlan`].
#[derive(Debug)]
pub struct CasePlan {
    id: Box<str>,
    package: CasePackage,
}

/// Builder for one generated hermetic evaluator case.
#[derive(Debug)]
pub struct HermeticCasePlan {
    id: Box<str>,
    case: HermeticCase,
}

#[derive(Debug)]
enum CasePackage {
    Existing(PathBuf),
    Hermetic(HermeticCase),
}

#[derive(Debug)]
struct HermeticCase {
    prompt: String,
    transcript: Vec<PromptMessage>,
    benchmark_prompt_chars: Option<u64>,
    benchmark_case_type: Option<String>,
    environment: Environment,
    environment_files: Vec<GeneratedFile>,
    harness: Harness,
    harness_files: Vec<GeneratedFile>,
    output: TaskOutput,
    resources: Resources,
    agent_timeout: Duration,
    agent_instructions: Option<String>,
    verifier_timeout: Duration,
    scoring_policy: ScoringPolicy,
    artifacts: Vec<HermeticArtifact>,
    allow_internet: bool,
    verifier_allow_internet: bool,
}

#[derive(Debug)]
struct HermeticArtifact {
    source: PathBuf,
    exclude: Vec<PathBuf>,
}

/// Agent environment used by a generated case.
#[derive(Clone, Debug)]
pub enum Environment {
    /// Pull a pinned or otherwise caller-selected OCI image.
    OciImage(String),
    /// Build an image from this Dockerfile context.
    Dockerfile(PathBuf),
}

/// Canonical verifier package used by a generated case.
#[derive(Clone, Debug)]
pub struct Harness {
    directory: PathBuf,
}

#[derive(Clone, Debug)]
struct GeneratedFile {
    path: PathBuf,
    contents: GeneratedFileContents,
    mode: u32,
}

#[derive(Clone, Debug)]
enum GeneratedFileContents {
    Inline(Vec<u8>),
    Source(PathBuf),
}

/// Failure to inspect, normalize, store, or reload an imported dataset.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// A source or generated package could not be read or written.
    #[error("dataset import I/O failed at {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },

    /// JSON or JSONL metadata was malformed.
    #[error("failed to decode dataset JSON at {path}: {source}")]
    Json {
        /// Affected path.
        path: PathBuf,
        /// Decoder failure.
        #[source]
        source: serde_json::Error,
    },

    /// A normalized task package was invalid.
    #[error("imported task package is invalid: {0}")]
    Task(#[from] TaskLoadError),

    /// Upstream metadata cannot be represented faithfully.
    #[error("invalid dataset import: {0}")]
    Invalid(String),

    /// The content-addressed destination exists with different contents.
    #[error("import destination collision at {0}")]
    Collision(PathBuf),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DatasetManifest {
    schema: Box<str>,
    name: Box<str>,
    source: SourceIdentity,
    digest: Box<str>,
    tasks: Vec<ManifestTask>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ManifestTask {
    id: Box<str>,
    path: Box<str>,
    digest: Box<str>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PlanIndex {
    schema: Box<str>,
    key: Box<str>,
    dataset: Box<str>,
}

#[derive(Serialize)]
struct DigestManifest<'a> {
    schema: &'a str,
    name: &'a str,
    source: &'a SourceIdentity,
    tasks: &'a [ManifestTask],
}

impl ImportStore {
    /// Creates a store rooted at `directory`.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            root: directory.into(),
        }
    }

    /// Imports or reopens one content-identical dataset.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when the importer rejects its source, a package
    /// cannot be materialized, or retained content fails validation.
    pub fn import(&self, importer: &impl DatasetImporter) -> Result<ImportedDataset, ImportError> {
        let plan = importer.plan()?;
        validate_component("dataset name", &plan.name)?;
        if plan.cases.is_empty() {
            return Err(ImportError::Invalid(
                "an imported dataset must contain at least one case".to_owned(),
            ));
        }
        create_dir_all(&self.root)?;
        let plan_key = plan.fingerprint()?;
        if let Some(dataset) = self.load_plan(&plan_key)? {
            return Ok(dataset);
        }
        let temporary = tempfile::Builder::new()
            .prefix(".import-")
            .tempdir_in(&self.root)
            .map_err(|source| io_error(&self.root, source))?;
        let staged = temporary.path().join("dataset");
        let tasks_root = staged.join("tasks");
        create_dir_all(&tasks_root)?;

        let mut ids = BTreeSet::new();
        let mut tasks = Vec::with_capacity(plan.cases.len());
        for case in plan.cases {
            let CasePlan { id, package } = case;
            validate_component("case id", &id)?;
            if !ids.insert(id.clone()) {
                return Err(ImportError::Invalid(format!("duplicate case id {:?}", id)));
            }
            let relative = PathBuf::from("tasks").join(id.as_ref());
            let destination = staged.join(&relative);
            match package {
                CasePackage::Existing(root) => {
                    let task = Task::load(root)?;
                    task.materialize_package(&destination)?;
                }
                CasePackage::Hermetic(case) => {
                    materialize_hermetic_case(&id, case, &destination)?;
                }
            }
            let task = Task::load(&destination)?;
            tasks.push(ManifestTask {
                id,
                path: normalized_relative(&relative)?.into_boxed_str(),
                digest: task.content_digest().to_owned().into_boxed_str(),
            });
        }

        let digest = digest_manifest(&plan.name, &plan.source, &tasks)?;
        let manifest = DatasetManifest {
            schema: IMPORT_SCHEMA.into(),
            name: plan.name.clone(),
            source: plan.source,
            digest: digest.clone().into_boxed_str(),
            tasks,
        };
        write_json(&staged.join(MANIFEST_FILE), &manifest)?;

        let dataset_parent = self.root.join(plan.name.as_ref());
        create_dir_all(&dataset_parent)?;
        let destination = dataset_parent.join(&digest);
        let imported = if destination.exists() {
            load_matching_dataset(&destination, &manifest)?
        } else if let Err(source) = fs::rename(&staged, &destination) {
            // Another importer may have published the same digest after the
            // existence check. Accept only an identical, fully valid result.
            if destination.exists() {
                load_matching_dataset(&destination, &manifest)?
            } else {
                return Err(io_error(&destination, source));
            }
        } else {
            sync_directory(&dataset_parent)?;
            ImportedDataset::load(destination)?
        };
        self.publish_plan(&plan_key, &imported)?;
        Ok(imported)
    }

    /// Returns the root containing all imported datasets.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn load_plan(&self, key: &str) -> Result<Option<ImportedDataset>, ImportError> {
        let path = self.root.join(".plans").join(format!("{key}.json"));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error(&path, source)),
        };
        let index: PlanIndex =
            serde_json::from_slice(&bytes).map_err(|source| ImportError::Json { path, source })?;
        if index.schema.as_ref() != PLAN_INDEX_SCHEMA || index.key.as_ref() != key {
            return Err(ImportError::Invalid(format!(
                "import plan index {key} has an incompatible identity"
            )));
        }
        let relative = safe_manifest_path(&index.dataset)?;
        Ok(Some(ImportedDataset::load(self.root.join(relative))?))
    }

    fn publish_plan(&self, key: &str, dataset: &ImportedDataset) -> Result<(), ImportError> {
        let directory = self.root.join(".plans");
        create_dir_all(&directory)?;
        let relative = dataset
            .root
            .strip_prefix(
                fs::canonicalize(&self.root).map_err(|source| io_error(&self.root, source))?,
            )
            .map_err(|error| ImportError::Invalid(error.to_string()))?;
        let relative = normalized_relative(relative)?;
        let index = PlanIndex {
            schema: PLAN_INDEX_SCHEMA.into(),
            key: key.into(),
            dataset: relative.into_boxed_str(),
        };
        let path = directory.join(format!("{key}.json"));
        if path.is_file() {
            let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
            let retained: PlanIndex =
                serde_json::from_slice(&bytes).map_err(|source| ImportError::Json {
                    path: path.clone(),
                    source,
                })?;
            if retained == index {
                return Ok(());
            }
            return Err(ImportError::Collision(path));
        }
        let temporary = tempfile::NamedTempFile::new_in(&directory)
            .map_err(|source| io_error(&directory, source))?;
        serde_json::to_writer_pretty(temporary.as_file(), &index).map_err(|source| {
            ImportError::Json {
                path: path.clone(),
                source,
            }
        })?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| io_error(&path, source))?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => sync_directory(&directory),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                self.publish_plan(key, dataset)
            }
            Err(error) => Err(io_error(&path, error.error)),
        }
    }
}

impl DatasetPlan {
    fn fingerprint(&self) -> Result<String, ImportError> {
        let mut digest = Sha256::new();
        Self::update_digest(&mut digest, PLAN_INDEX_SCHEMA.as_bytes());
        Self::update_digest(&mut digest, self.name.as_bytes());
        Self::update_digest(&mut digest, self.source.kind.as_bytes());
        Self::update_digest(&mut digest, self.source.revision.as_bytes());
        Self::update_digest(&mut digest, self.source.digest.as_bytes());
        for case in &self.cases {
            Self::update_digest(&mut digest, case.id.as_bytes());
            match &case.package {
                CasePackage::Existing(root) => {
                    Self::update_digest(&mut digest, b"existing");
                    let task = Task::load(root)?;
                    Self::update_digest(&mut digest, task.content_digest().as_bytes());
                }
                CasePackage::Hermetic(case) => {
                    Self::update_digest(&mut digest, b"hermetic");
                    Self::update_digest(&mut digest, case.prompt.as_bytes());
                    Self::update_digest(
                        &mut digest,
                        &serde_json::to_vec(&case.transcript).map_err(|source| {
                            ImportError::Json {
                                path: PathBuf::from("<import-plan>"),
                                source,
                            }
                        })?,
                    );
                    Self::update_digest(
                        &mut digest,
                        &case
                            .benchmark_prompt_chars
                            .unwrap_or_default()
                            .to_le_bytes(),
                    );
                    Self::update_digest(
                        &mut digest,
                        case.benchmark_case_type.as_deref().unwrap_or("").as_bytes(),
                    );
                    match &case.environment {
                        Environment::OciImage(image) => {
                            Self::update_digest(&mut digest, b"oci");
                            Self::update_digest(&mut digest, image.as_bytes());
                        }
                        Environment::Dockerfile(root) => {
                            Self::update_digest(&mut digest, b"dockerfile");
                            Self::update_directory(&mut digest, root)?;
                        }
                    }
                    for file in &case.environment_files {
                        Self::update_digest(
                            &mut digest,
                            normalized_relative(&file.path)?.as_bytes(),
                        );
                        Self::update_digest(&mut digest, &file.mode.to_le_bytes());
                        Self::update_generated_file(&mut digest, file)?;
                    }
                    Self::update_directory(&mut digest, &case.harness.directory)?;
                    for file in &case.harness_files {
                        Self::update_digest(
                            &mut digest,
                            normalized_relative(&file.path)?.as_bytes(),
                        );
                        Self::update_digest(&mut digest, &file.mode.to_le_bytes());
                        Self::update_generated_file(&mut digest, file)?;
                    }
                    Self::update_digest(
                        &mut digest,
                        serde_json::to_string(&case.output)
                            .map_err(|source| ImportError::Json {
                                path: PathBuf::from("<import-plan>"),
                                source,
                            })?
                            .as_bytes(),
                    );
                    Self::update_digest(&mut digest, &case.resources.cpus.to_le_bytes());
                    Self::update_digest(&mut digest, &case.resources.memory_mb.to_le_bytes());
                    Self::update_digest(&mut digest, &case.resources.storage_mb.to_le_bytes());
                    Self::update_digest(&mut digest, &case.resources.gpus.to_le_bytes());
                    Self::update_digest(&mut digest, &case.agent_timeout.as_nanos().to_le_bytes());
                    Self::update_digest(
                        &mut digest,
                        &case.verifier_timeout.as_nanos().to_le_bytes(),
                    );
                    Self::update_digest(&mut digest, case.scoring_policy.as_str().as_bytes());
                    for artifact in &case.artifacts {
                        Self::update_digest(
                            &mut digest,
                            artifact.source.to_string_lossy().as_bytes(),
                        );
                        for excluded in &artifact.exclude {
                            Self::update_digest(
                                &mut digest,
                                normalized_relative(excluded)?.as_bytes(),
                            );
                        }
                    }
                    Self::update_digest(
                        &mut digest,
                        case.agent_instructions.as_deref().unwrap_or("").as_bytes(),
                    );
                    Self::update_digest(&mut digest, &[u8::from(case.allow_internet)]);
                    Self::update_digest(&mut digest, &[u8::from(case.verifier_allow_internet)]);
                }
            }
        }
        Ok(hex::encode(digest.finalize()))
    }

    fn update_directory(digest: &mut Sha256, root: &Path) -> Result<(), ImportError> {
        let root = fs::canonicalize(root).map_err(|source| io_error(root, source))?;
        let mut entries = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_exclude(false)
            .parents(false)
            .follow_links(false)
            .build()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ImportError::Invalid(error.to_string()))?;
        entries.sort_unstable_by(|left, right| left.path().cmp(right.path()));
        for entry in entries {
            if entry.path() == root {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&root)
                .map_err(|error| ImportError::Invalid(error.to_string()))?;
            Self::update_digest(digest, normalized_relative(relative)?.as_bytes());
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|source| io_error(entry.path(), source))?;
            Self::update_digest(digest, &metadata.permissions().mode().to_le_bytes());
            if metadata.is_file() {
                Self::update_digest(digest, b"file");
                let bytes =
                    fs::read(entry.path()).map_err(|source| io_error(entry.path(), source))?;
                Self::update_digest(digest, &bytes);
            } else if metadata.is_dir() {
                Self::update_digest(digest, b"directory");
            } else {
                return Err(ImportError::Invalid(format!(
                    "unsupported import-plan entry: {}",
                    entry.path().display()
                )));
            }
        }
        Ok(())
    }

    fn update_generated_file(digest: &mut Sha256, file: &GeneratedFile) -> Result<(), ImportError> {
        match &file.contents {
            GeneratedFileContents::Inline(bytes) => {
                Self::update_digest(digest, bytes);
                Ok(())
            }
            GeneratedFileContents::Source(path) => {
                let mut source = fs::File::open(path).map_err(|error| io_error(path, error))?;
                let length = source
                    .metadata()
                    .map_err(|error| io_error(path, error))?
                    .len();
                digest.update(length.to_le_bytes());
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = source
                        .read(&mut buffer)
                        .map_err(|error| io_error(path, error))?;
                    if read == 0 {
                        break;
                    }
                    digest.update(&buffer[..read]);
                }
                Ok(())
            }
        }
    }

    fn update_digest(digest: &mut Sha256, value: &[u8]) {
        digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(value);
    }
}

fn load_matching_dataset(
    destination: &Path,
    expected: &DatasetManifest,
) -> Result<ImportedDataset, ImportError> {
    let existing = ImportedDataset::load(destination)?;
    if &existing.manifest()? != expected {
        return Err(ImportError::Collision(destination.to_path_buf()));
    }
    Ok(existing)
}

impl ImportedDataset {
    /// Loads and validates a previously imported dataset.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when the manifest or any immutable task package
    /// is missing, malformed, or content-inconsistent.
    pub fn load(directory: impl AsRef<Path>) -> Result<Self, ImportError> {
        let requested = directory.as_ref();
        let root = fs::canonicalize(requested).map_err(|source| io_error(requested, source))?;
        let manifest = read_manifest(&root)?;
        if manifest.schema.as_ref() != IMPORT_SCHEMA {
            return Err(ImportError::Invalid(format!(
                "unsupported import schema {:?}",
                manifest.schema
            )));
        }
        let expected = digest_manifest(&manifest.name, &manifest.source, &manifest.tasks)?;
        if manifest.digest.as_ref() != expected {
            return Err(ImportError::Invalid(format!(
                "dataset digest mismatch: manifest has {}, computed {expected}",
                manifest.digest
            )));
        }
        let mut tasks = Vec::with_capacity(manifest.tasks.len());
        for entry in &manifest.tasks {
            let relative = safe_manifest_path(&entry.path)?;
            let task = Task::load(root.join(relative))?;
            if task.content_digest() != entry.digest.as_ref() {
                return Err(ImportError::Invalid(format!(
                    "task {} digest mismatch",
                    entry.id
                )));
            }
            tasks.push(task.attach_dataset(&manifest.name, Some(manifest.source.revision())));
        }
        Ok(Self {
            root,
            digest: manifest.digest,
            source: manifest.source,
            tasks,
        })
    }

    /// Returns the immutable dataset root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the content digest covering source identity and every task.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns safe upstream provenance.
    #[must_use]
    pub const fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// Returns evaluator-ready immutable tasks in source order.
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    fn manifest(&self) -> Result<DatasetManifest, ImportError> {
        read_manifest(&self.root)
    }
}

impl DatasetPlan {
    /// Creates an empty plan with stable source provenance.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when the dataset name is unsafe.
    pub fn new(name: impl Into<String>, source: SourceIdentity) -> Result<Self, ImportError> {
        let name = name.into();
        validate_component("dataset name", &name)?;
        Ok(Self {
            name: name.into_boxed_str(),
            source,
            cases: Vec::new(),
        })
    }

    /// Appends one case in stable source order.
    #[must_use]
    pub fn case(mut self, case: impl Into<CasePlan>) -> Self {
        self.cases.push(case.into());
        self
    }
}

impl SourceIdentity {
    /// Creates source provenance without retaining a local path or credential.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] for an empty kind/revision or a digest other
    /// than lowercase SHA-256.
    pub fn new(
        kind: impl Into<String>,
        revision: impl Into<String>,
        digest: impl Into<String>,
    ) -> Result<Self, ImportError> {
        let kind = kind.into();
        let revision = revision.into();
        let digest = digest.into();
        if kind.trim().is_empty() || revision.trim().is_empty() {
            return Err(ImportError::Invalid(
                "source kind and revision must not be empty".to_owned(),
            ));
        }
        if !is_sha256(&digest) {
            return Err(ImportError::Invalid(
                "source digest must be a lowercase SHA-256 digest".to_owned(),
            ));
        }
        Ok(Self {
            kind: kind.into_boxed_str(),
            revision: revision.into_boxed_str(),
            digest: digest.into_boxed_str(),
        })
    }

    /// Returns the stable importer/source kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the caller-pinned upstream revision.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Returns the SHA-256 of consumed source metadata.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl CasePlan {
    /// Snapshots an existing evaluator task package without semantic changes.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when `id` is unsafe.
    pub fn existing(
        id: impl Into<String>,
        package: impl Into<PathBuf>,
    ) -> Result<Self, ImportError> {
        let id = id.into();
        validate_component("case id", &id)?;
        Ok(Self {
            id: id.into_boxed_str(),
            package: CasePackage::Existing(package.into()),
        })
    }

    /// Creates a generated hermetic case.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when `id` is unsafe.
    pub fn hermetic(
        id: impl Into<String>,
        prompt: impl Into<String>,
        environment: Environment,
        harness: Harness,
    ) -> Result<HermeticCasePlan, ImportError> {
        let id = id.into();
        validate_component("case id", &id)?;
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(ImportError::Invalid(format!(
                "case {id:?} has an empty prompt"
            )));
        }
        Ok(HermeticCasePlan {
            id: id.into_boxed_str(),
            case: HermeticCase {
                prompt,
                transcript: Vec::new(),
                benchmark_prompt_chars: None,
                benchmark_case_type: None,
                environment,
                environment_files: Vec::new(),
                harness,
                harness_files: Vec::new(),
                output: TaskOutput::Workspace,
                resources: Resources {
                    cpus: 2,
                    memory_mb: 4096,
                    storage_mb: 10_240,
                    gpus: 0,
                },
                agent_timeout: Duration::from_secs(900),
                agent_instructions: None,
                verifier_timeout: Duration::from_secs(300),
                scoring_policy: ScoringPolicy::AllRewardsPositive,
                artifacts: Vec::new(),
                allow_internet: true,
                verifier_allow_internet: true,
            },
        })
    }
}

impl HermeticCasePlan {
    /// Prepends a synthetic text-only conversation to the final user prompt.
    #[must_use]
    pub fn transcript(mut self, messages: impl IntoIterator<Item = PromptMessage>) -> Self {
        self.case.transcript = messages.into_iter().collect();
        self
    }

    /// Retains the source benchmark's prompt-size dimension for official binning.
    #[must_use]
    pub const fn benchmark_prompt_chars(mut self, chars: u64) -> Self {
        self.case.benchmark_prompt_chars = Some(chars);
        self
    }

    /// Retains the source benchmark's case-type dimension for official grouping.
    #[must_use]
    pub fn benchmark_case_type(mut self, case_type: impl Into<String>) -> Self {
        self.case.benchmark_case_type = Some(case_type.into());
        self
    }

    /// Adds one case-specific file to the candidate environment.
    ///
    /// Native attempts copy this file into the disposable workspace. A
    /// Dockerfile-backed VM environment may place it in the guest workspace
    /// with an ordinary `COPY` instruction.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when `path` is not a safe relative path.
    pub fn environment_file(
        mut self,
        path: impl Into<PathBuf>,
        bytes: impl Into<Vec<u8>>,
        mode: u32,
    ) -> Result<Self, ImportError> {
        self.case.environment_files.push(GeneratedFile::inline(
            path.into(),
            bytes.into(),
            mode,
            "environment",
        )?);
        Ok(self)
    }

    /// Snapshots one existing file into the candidate environment without
    /// buffering its contents in the import plan.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when either path is unsafe or the source is not
    /// an ordinary file.
    pub fn environment_file_from(
        mut self,
        path: impl Into<PathBuf>,
        source: impl Into<PathBuf>,
        mode: u32,
    ) -> Result<Self, ImportError> {
        self.case.environment_files.push(GeneratedFile::source(
            path.into(),
            source.into(),
            mode,
            "environment",
        )?);
        Ok(self)
    }

    /// Applies benchmark-owned model instructions separately from the user prompt.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.case.agent_instructions = Some(instructions.into());
        self
    }

    /// Selects which candidate value the verifier consumes.
    #[must_use]
    pub const fn output(mut self, output: TaskOutput) -> Self {
        self.case.output = output;
        self
    }

    /// Selects task resource admission and VM sizing.
    #[must_use]
    pub const fn resources(mut self, resources: Resources) -> Self {
        self.case.resources = resources;
        self
    }

    /// Selects agent and verifier deadlines.
    #[must_use]
    pub const fn timeouts(mut self, agent: Duration, verifier: Duration) -> Self {
        self.case.agent_timeout = agent;
        self.case.verifier_timeout = verifier;
        self
    }

    /// Selects how numeric verifier rewards map to binary pass/fail status.
    #[must_use]
    pub const fn scoring_policy(mut self, policy: ScoringPolicy) -> Self {
        self.case.scoring_policy = policy;
        self
    }

    /// Captures one absolute guest path after agent execution for a separate verifier.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when the source is not a safe absolute guest path.
    pub fn artifact(mut self, source: impl Into<PathBuf>) -> Result<Self, ImportError> {
        let source = source.into();
        if source.to_str().is_none() || !safe_absolute_guest_path(&source) {
            return Err(ImportError::Invalid(format!(
                "unsafe generated artifact source: {}",
                source.display()
            )));
        }
        self.case.artifacts.push(HermeticArtifact {
            source,
            exclude: Vec::new(),
        });
        Ok(self)
    }

    /// Selects whether the agent environment has a network device.
    #[must_use]
    pub const fn allow_internet(mut self, allow: bool) -> Self {
        self.case.allow_internet = allow;
        self.case.verifier_allow_internet = allow;
        self
    }

    /// Selects whether a separate verifier environment has a network device.
    ///
    /// This may differ from the agent policy only when the harness supplies a
    /// separate verifier Dockerfile.
    #[must_use]
    pub const fn verifier_allow_internet(mut self, allow: bool) -> Self {
        self.case.verifier_allow_internet = allow;
        self
    }

    /// Adds one case-specific file to the canonical verifier package.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when `path` is not a safe relative path.
    pub fn harness_file(
        mut self,
        path: impl Into<PathBuf>,
        bytes: impl Into<Vec<u8>>,
        mode: u32,
    ) -> Result<Self, ImportError> {
        self.case.harness_files.push(GeneratedFile::inline(
            path.into(),
            bytes.into(),
            mode,
            "harness",
        )?);
        Ok(self)
    }

    /// Snapshots one existing file into the verifier package without
    /// buffering its contents in the import plan.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when either path is unsafe or the source is not
    /// an ordinary file.
    pub fn harness_file_from(
        mut self,
        path: impl Into<PathBuf>,
        source: impl Into<PathBuf>,
        mode: u32,
    ) -> Result<Self, ImportError> {
        self.case.harness_files.push(GeneratedFile::source(
            path.into(),
            source.into(),
            mode,
            "harness",
        )?);
        Ok(self)
    }
}

impl GeneratedFile {
    fn inline(path: PathBuf, bytes: Vec<u8>, mode: u32, kind: &str) -> Result<Self, ImportError> {
        Self::new(path, GeneratedFileContents::Inline(bytes), mode, kind)
    }

    fn source(path: PathBuf, source: PathBuf, mode: u32, kind: &str) -> Result<Self, ImportError> {
        if !source.is_file() {
            return Err(ImportError::Invalid(format!(
                "generated {kind} source is not a file: {}",
                source.display()
            )));
        }
        Self::new(path, GeneratedFileContents::Source(source), mode, kind)
    }

    fn new(
        path: PathBuf,
        contents: GeneratedFileContents,
        mode: u32,
        kind: &str,
    ) -> Result<Self, ImportError> {
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(ImportError::Invalid(format!(
                "unsafe generated {kind} path: {}",
                path.display()
            )));
        }
        Ok(Self {
            path,
            contents,
            mode,
        })
    }
}

impl From<HermeticCasePlan> for CasePlan {
    fn from(value: HermeticCasePlan) -> Self {
        Self {
            id: value.id,
            package: CasePackage::Hermetic(value.case),
        }
    }
}

impl Harness {
    /// Uses a directory containing canonical `test.sh` and optional
    /// `Dockerfile` verifier inputs.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError`] when the directory lacks `test.sh`.
    pub fn directory(directory: impl Into<PathBuf>) -> Result<Self, ImportError> {
        let directory = directory.into();
        let script = directory.join("test.sh");
        if !script.is_file() {
            return Err(ImportError::Invalid(format!(
                "harness is missing {}",
                script.display()
            )));
        }
        Ok(Self { directory })
    }
}

fn materialize_hermetic_case(
    id: &str,
    case: HermeticCase,
    destination: &Path,
) -> Result<(), ImportError> {
    create_dir_all(destination)?;
    fs::write(destination.join("instruction.md"), case.prompt)
        .map_err(|source| io_error(destination.join("instruction.md"), source))?;
    if !case.transcript.is_empty() {
        write_json(
            &destination.join("transcript.json"),
            &TaskTranscript {
                messages: &case.transcript,
            },
        )?;
    }
    let environment = destination.join("environment");
    let image = match &case.environment {
        Environment::OciImage(image) => Some(image.clone()),
        Environment::Dockerfile(_) => None,
    };
    match case.environment {
        Environment::OciImage(_) => create_dir_all(&environment)?,
        Environment::Dockerfile(source) => copy_tree(&source, &environment)?,
    }
    materialize_generated_files(case.environment_files, &environment)?;
    materialize_harness(case.harness, &destination.join("tests"))?;
    materialize_generated_files(case.harness_files, &destination.join("tests"))?;

    let separate = destination.join("tests/Dockerfile").is_file();
    let mut artifacts = case
        .artifacts
        .iter()
        .map(|artifact| GeneratedTaskArtifact {
            source: &artifact.source,
            exclude: &artifact.exclude,
        })
        .collect::<Vec<_>>();
    let workspace_excludes = [
        PathBuf::from("Dockerfile"),
        PathBuf::from("instruction.md"),
        PathBuf::from("reference_files"),
        PathBuf::from(".git"),
        PathBuf::from(".nanocodex"),
    ];
    if separate && case.output == TaskOutput::Workspace {
        artifacts.push(GeneratedTaskArtifact {
            source: Path::new("/workspace"),
            exclude: &workspace_excludes,
        });
    }
    let raw = GeneratedTaskManifest {
        schema_version: "1.3",
        output: match case.output {
            TaskOutput::Workspace => "workspace",
            TaskOutput::FinalMessage => "final_message",
        },
        task: GeneratedTaskInfo {
            name: id,
            benchmark_prompt_chars: case.benchmark_prompt_chars,
            benchmark_case_type: case.benchmark_case_type.as_deref(),
        },
        agent: GeneratedPhase {
            timeout_sec: case.agent_timeout.as_secs_f64(),
            instructions: case.agent_instructions.as_deref(),
        },
        verifier: GeneratedVerifier {
            timeout_sec: case.verifier_timeout.as_secs_f64(),
            environment_mode: if separate { "separate" } else { "same" },
            scoring_policy: case.scoring_policy,
            network_mode: (case.verifier_allow_internet != case.allow_internet).then_some(
                if case.verifier_allow_internet {
                    "public"
                } else {
                    "no-network"
                },
            ),
        },
        environment: GeneratedEnvironment {
            docker_image: image.as_deref(),
            cpus: case.resources.cpus,
            memory_mb: case.resources.memory_mb,
            storage_mb: case.resources.storage_mb,
            gpus: case.resources.gpus,
            allow_internet: case.allow_internet,
        },
        artifacts: (!artifacts.is_empty()).then_some(artifacts.as_slice()),
    };
    let manifest = toml::to_string(&raw).map_err(|source| {
        ImportError::Invalid(format!("failed to encode generated task {id}: {source}"))
    })?;
    fs::write(destination.join("task.toml"), manifest)
        .map_err(|source| io_error(destination.join("task.toml"), source))?;
    Ok(())
}

#[derive(Serialize)]
struct TaskTranscript<'a> {
    messages: &'a [PromptMessage],
}

fn materialize_harness(harness: Harness, destination: &Path) -> Result<(), ImportError> {
    copy_tree(&harness.directory, destination)
}

fn materialize_generated_files(
    files: Vec<GeneratedFile>,
    destination: &Path,
) -> Result<(), ImportError> {
    for file in files {
        let path = destination.join(file.path);
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        match file.contents {
            GeneratedFileContents::Inline(bytes) => {
                fs::write(&path, bytes).map_err(|source| io_error(&path, source))?;
            }
            GeneratedFileContents::Source(source) => {
                fs::copy(&source, &path).map_err(|error| io_error(&source, error))?;
            }
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(file.mode))
            .map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct GeneratedTaskManifest<'a> {
    schema_version: &'a str,
    output: &'a str,
    task: GeneratedTaskInfo<'a>,
    agent: GeneratedPhase<'a>,
    verifier: GeneratedVerifier<'a>,
    environment: GeneratedEnvironment<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifacts: Option<&'a [GeneratedTaskArtifact<'a>]>,
}

#[derive(Serialize)]
struct GeneratedTaskArtifact<'a> {
    source: &'a Path,
    exclude: &'a [PathBuf],
}

#[derive(Serialize)]
struct GeneratedTaskInfo<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    benchmark_prompt_chars: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    benchmark_case_type: Option<&'a str>,
}

#[derive(Serialize)]
struct GeneratedPhase<'a> {
    timeout_sec: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<&'a str>,
}

#[derive(Serialize)]
struct GeneratedVerifier<'a> {
    timeout_sec: f64,
    environment_mode: &'a str,
    scoring_policy: ScoringPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_mode: Option<&'a str>,
}

#[derive(Serialize)]
struct GeneratedEnvironment<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    docker_image: Option<&'a str>,
    cpus: u32,
    memory_mb: u64,
    storage_mb: u64,
    gpus: u32,
    allow_internet: bool,
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), ImportError> {
    let source = fs::canonicalize(source).map_err(|error| io_error(source, error))?;
    if !source.is_dir() {
        return Err(ImportError::Invalid(format!(
            "package root is not a directory: {}",
            source.display()
        )));
    }
    create_dir_all(destination)?;
    let mut directories = Vec::new();
    for entry in ignore::WalkBuilder::new(&source)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .build()
    {
        let entry = entry.map_err(|error| {
            ImportError::Invalid(format!("failed to walk {}: {error}", source.display()))
        })?;
        if entry.path() == source {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&source)
            .map_err(|error| ImportError::Invalid(error.to_string()))?;
        let target = destination.join(relative);
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|error| io_error(entry.path(), error))?;
        if metadata.file_type().is_symlink() {
            return Err(ImportError::Invalid(format!(
                "package symlinks are unsupported: {}",
                entry.path().display()
            )));
        }
        if metadata.is_dir() {
            create_dir_all(&target)?;
            directories.push((target, metadata.permissions().mode()));
        } else if metadata.is_file() {
            fs::copy(entry.path(), &target).map_err(|error| io_error(&target, error))?;
            fs::set_permissions(
                &target,
                fs::Permissions::from_mode(metadata.permissions().mode()),
            )
            .map_err(|error| io_error(&target, error))?;
        } else {
            return Err(ImportError::Invalid(format!(
                "unsupported package entry: {}",
                entry.path().display()
            )));
        }
    }
    for (directory, mode) in directories.into_iter().rev() {
        fs::set_permissions(&directory, fs::Permissions::from_mode(mode))
            .map_err(|error| io_error(&directory, error))?;
    }
    Ok(())
}

fn read_manifest(root: &Path) -> Result<DatasetManifest, ImportError> {
    let path = root.join(MANIFEST_FILE);
    let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
    serde_json::from_slice(&bytes).map_err(|source| ImportError::Json { path, source })
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ImportError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| ImportError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(path, source))
}

fn digest_manifest(
    name: &str,
    source: &SourceIdentity,
    tasks: &[ManifestTask],
) -> Result<String, ImportError> {
    let bytes = serde_json::to_vec(&DigestManifest {
        schema: IMPORT_SCHEMA,
        name,
        source,
        tasks,
    })
    .map_err(|source| ImportError::Json {
        path: PathBuf::from("<import-digest>"),
        source,
    })?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn safe_manifest_path(path: &str) -> Result<PathBuf, ImportError> {
    let path = PathBuf::from(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ImportError::Invalid(format!(
            "unsafe task path in import manifest: {}",
            path.display()
        )));
    }
    Ok(path)
}

fn normalized_relative(path: &Path) -> Result<String, ImportError> {
    safe_manifest_path(&path.to_string_lossy())?;
    Ok(path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn safe_absolute_guest_path(path: &Path) -> bool {
    path.is_absolute()
        && path.strip_prefix("/").is_ok_and(|relative| {
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
        })
}

fn validate_component(label: &str, value: &str) -> Result<(), ImportError> {
    let valid = !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'));
    if valid {
        Ok(())
    } else {
        Err(ImportError::Invalid(format!(
            "{label} {value:?} is not a safe path component"
        )))
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn create_dir_all(path: impl AsRef<Path>) -> Result<(), ImportError> {
    let path = path.as_ref();
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

fn sync_directory(path: &Path) -> Result<(), ImportError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn io_error(path: impl AsRef<Path>, source: io::Error) -> ImportError {
    ImportError::Io {
        path: path.as_ref().to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use crate::ScoringPolicy;

    use super::{
        CasePackage, CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError,
        ImportStore, SourceIdentity, materialize_hermetic_case,
    };

    struct FixtureImporter {
        task: std::path::PathBuf,
        revision: &'static str,
    }

    struct WorkspaceImporter<'a> {
        environment: &'a Path,
        harness: &'a Path,
        contents: &'a [u8],
        scoring_policy: ScoringPolicy,
        benchmark_prompt_chars: u64,
        benchmark_case_type: &'a str,
    }

    struct PathWorkspaceImporter<'a> {
        environment: &'a Path,
        harness: &'a Path,
        candidate: &'a Path,
        verifier: &'a Path,
    }

    impl DatasetImporter for FixtureImporter {
        fn plan(&self) -> Result<DatasetPlan, ImportError> {
            Ok(DatasetPlan::new(
                "fixture",
                SourceIdentity::new("fixture", self.revision, "a".repeat(64))?,
            )?
            .case(CasePlan::existing("case-1", &self.task)?))
        }
    }

    impl DatasetImporter for WorkspaceImporter<'_> {
        fn plan(&self) -> Result<DatasetPlan, ImportError> {
            Ok(DatasetPlan::new(
                "workspace-input",
                SourceIdentity::new("fixture", "fixture@1", "b".repeat(64))?,
            )?
            .case(
                CasePlan::hermetic(
                    "case-1",
                    "Inspect the supplied data.",
                    Environment::Dockerfile(self.environment.to_path_buf()),
                    Harness::directory(self.harness)?,
                )?
                .benchmark_prompt_chars(self.benchmark_prompt_chars)
                .benchmark_case_type(self.benchmark_case_type)
                .scoring_policy(self.scoring_policy)
                .environment_file("data_files/input.csv", self.contents, 0o644)?,
            ))
        }
    }

    impl DatasetImporter for PathWorkspaceImporter<'_> {
        fn plan(&self) -> Result<DatasetPlan, ImportError> {
            Ok(DatasetPlan::new(
                "path-workspace-input",
                SourceIdentity::new("fixture", "fixture@1", "c".repeat(64))?,
            )?
            .case(
                CasePlan::hermetic(
                    "case-1",
                    "Inspect the supplied data.",
                    Environment::Dockerfile(self.environment.to_path_buf()),
                    Harness::directory(self.harness)?,
                )?
                .environment_file_from("data_files/input.bin", self.candidate, 0o640)?
                .harness_file_from("expected.bin", self.verifier, 0o600)?,
            ))
        }
    }

    #[test]
    fn imports_content_addressed_tasks_and_reopens_identical_content() {
        let source = tempdir().unwrap();
        make_task(source.path(), "first");
        let store = tempdir().unwrap();
        let importer = FixtureImporter {
            task: source.path().to_path_buf(),
            revision: "upstream@1",
        };

        let first = ImportStore::new(store.path()).import(&importer).unwrap();
        let second = ImportStore::new(store.path()).import(&importer).unwrap();

        assert_eq!(first.root(), second.root());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.tasks().len(), 1);
        assert_eq!(first.tasks()[0].prompt(), "first");
        assert_eq!(first.tasks()[0].dataset(), Some("fixture"));
        assert_eq!(first.tasks()[0].dataset_revision(), Some("upstream@1"));
        let manifest = fs::read_to_string(first.root().join("dataset.json")).unwrap();
        assert!(!manifest.contains(&source.path().display().to_string()));

        fs::write(source.path().join("instruction.md"), "changed").unwrap();
        let changed = ImportStore::new(store.path()).import(&importer).unwrap();
        assert_ne!(first.digest(), changed.digest());
        assert_ne!(first.root(), changed.root());
    }

    #[test]
    fn generated_environment_files_are_candidate_inputs_and_affect_identity() {
        let source = tempdir().unwrap();
        fs::write(
            source.path().join("Dockerfile"),
            "FROM debian:bookworm-slim\nCOPY data_files /workspace/data_files\n",
        )
        .unwrap();
        let harness = tempdir().unwrap();
        fs::write(
            harness.path().join("test.sh"),
            "#!/bin/sh\nprintf '1\\n' > \"$NANOCODEX_EVAL_VERIFIER_LOGS/reward.txt\"\n",
        )
        .unwrap();
        let store = tempdir().unwrap();

        let first = ImportStore::new(store.path())
            .import(&WorkspaceImporter {
                environment: source.path(),
                harness: harness.path(),
                contents: b"value\n1\n",
                scoring_policy: ScoringPolicy::AllRewardsOne,
                benchmark_prompt_chars: 21,
                benchmark_case_type: "fixture",
            })
            .unwrap();
        let changed = ImportStore::new(store.path())
            .import(&WorkspaceImporter {
                environment: source.path(),
                harness: harness.path(),
                contents: b"value\n2\n",
                scoring_policy: ScoringPolicy::AllRewardsOne,
                benchmark_prompt_chars: 21,
                benchmark_case_type: "fixture",
            })
            .unwrap();

        assert_eq!(
            fs::read(
                first.tasks()[0]
                    .root()
                    .join("environment/data_files/input.csv")
            )
            .unwrap(),
            b"value\n1\n"
        );
        assert_eq!(
            first.tasks()[0].verifier().scoring_policy(),
            ScoringPolicy::AllRewardsOne
        );
        assert_eq!(first.tasks()[0].benchmark_prompt_chars(), Some(21));
        assert_eq!(first.tasks()[0].benchmark_case_type(), Some("fixture"));
        assert_ne!(first.digest(), changed.digest());

        let changed_policy = ImportStore::new(store.path())
            .import(&WorkspaceImporter {
                environment: source.path(),
                harness: harness.path(),
                contents: b"value\n1\n",
                scoring_policy: ScoringPolicy::AllRewardsPositive,
                benchmark_prompt_chars: 21,
                benchmark_case_type: "fixture",
            })
            .unwrap();
        assert_ne!(first.digest(), changed_policy.digest());

        let changed_dimensions = ImportStore::new(store.path())
            .import(&WorkspaceImporter {
                environment: source.path(),
                harness: harness.path(),
                contents: b"value\n1\n",
                scoring_policy: ScoringPolicy::AllRewardsOne,
                benchmark_prompt_chars: 22,
                benchmark_case_type: "other",
            })
            .unwrap();
        assert_ne!(first.digest(), changed_dimensions.digest());
    }

    #[test]
    fn generated_workspace_output_is_transferred_to_a_separate_verifier() {
        let environment = tempdir().unwrap();
        fs::write(
            environment.path().join("Dockerfile"),
            "FROM debian:bookworm-slim\nWORKDIR /workspace\n",
        )
        .unwrap();
        let harness = tempdir().unwrap();
        fs::write(
            harness.path().join("Dockerfile"),
            "FROM debian:bookworm-slim\nWORKDIR /workspace\n",
        )
        .unwrap();
        fs::write(
            harness.path().join("test.sh"),
            "#!/bin/sh\nprintf '1\\n' > \"$NANOCODEX_EVAL_VERIFIER_LOGS/reward.txt\"\n",
        )
        .unwrap();
        let store = tempdir().unwrap();

        let imported = ImportStore::new(store.path())
            .import(&WorkspaceImporter {
                environment: environment.path(),
                harness: harness.path(),
                contents: b"value\n1\n",
                scoring_policy: ScoringPolicy::AllRewardsPositive,
                benchmark_prompt_chars: 21,
                benchmark_case_type: "fixture",
            })
            .unwrap();
        let task = &imported.tasks()[0];

        assert_eq!(task.artifacts().len(), 1);
        assert_eq!(task.artifacts()[0].source(), Path::new("/workspace"));
        assert_eq!(
            task.artifacts()[0].exclude(),
            &[
                Path::new("Dockerfile").to_path_buf(),
                Path::new("instruction.md").to_path_buf(),
                Path::new("reference_files").to_path_buf(),
                Path::new(".git").to_path_buf(),
                Path::new(".nanocodex").to_path_buf(),
            ]
        );
    }

    #[test]
    fn generated_case_can_capture_an_explicit_guest_artifact() {
        let environment = tempdir().unwrap();
        fs::write(
            environment.path().join("Dockerfile"),
            "FROM debian:bookworm-slim\nWORKDIR /workspace\n",
        )
        .unwrap();
        let harness = tempdir().unwrap();
        fs::write(
            harness.path().join("Dockerfile"),
            "FROM debian:bookworm-slim\n",
        )
        .unwrap();
        fs::write(
            harness.path().join("test.sh"),
            "#!/bin/sh\nprintf '1\\n' > /logs/verifier/reward.txt\n",
        )
        .unwrap();
        let plan = DatasetPlan::new(
            "artifact-input",
            SourceIdentity::new("fixture", "fixture@1", "d".repeat(64)).unwrap(),
        )
        .unwrap()
        .case(
            CasePlan::hermetic(
                "case-1",
                "Write the answer artifact.",
                Environment::Dockerfile(environment.path().to_path_buf()),
                Harness::directory(harness.path()).unwrap(),
            )
            .unwrap()
            .output(crate::TaskOutput::FinalMessage)
            .artifact("/logs/agent/answer.txt")
            .unwrap(),
        );
        let destination = tempdir().unwrap();
        let temporary = tempfile::Builder::new()
            .prefix("artifact-case-")
            .tempdir_in(destination.path())
            .unwrap();
        let CasePackage::Hermetic(case) = plan.cases.into_iter().next().unwrap().package else {
            panic!("expected generated case");
        };
        materialize_hermetic_case("case-1", case, temporary.path()).unwrap();
        let task = crate::Task::load(temporary.path()).unwrap();

        assert_eq!(task.artifacts().len(), 1);
        assert_eq!(
            task.artifacts()[0].source(),
            Path::new("/logs/agent/answer.txt")
        );
    }

    #[test]
    fn path_backed_generated_files_are_snapshotted_and_affect_identity() {
        let source = tempdir().unwrap();
        fs::write(
            source.path().join("Dockerfile"),
            "FROM debian:bookworm-slim\nCOPY data_files /workspace/data_files\n",
        )
        .unwrap();
        let harness = tempdir().unwrap();
        fs::write(
            harness.path().join("test.sh"),
            "#!/bin/sh\nprintf '1\\n' > \"$NANOCODEX_EVAL_VERIFIER_LOGS/reward.txt\"\n",
        )
        .unwrap();
        let inputs = tempdir().unwrap();
        let candidate = inputs.path().join("candidate.bin");
        let verifier = inputs.path().join("verifier.bin");
        fs::write(&candidate, b"candidate-v1").unwrap();
        fs::write(&verifier, b"verifier-v1").unwrap();
        let store = tempdir().unwrap();
        let first = ImportStore::new(store.path())
            .import(&PathWorkspaceImporter {
                environment: source.path(),
                harness: harness.path(),
                candidate: &candidate,
                verifier: &verifier,
            })
            .unwrap();

        assert_eq!(
            fs::read(
                first.tasks()[0]
                    .root()
                    .join("environment/data_files/input.bin")
            )
            .unwrap(),
            b"candidate-v1"
        );
        assert_eq!(
            fs::read(first.tasks()[0].root().join("tests/expected.bin")).unwrap(),
            b"verifier-v1"
        );

        fs::write(&candidate, b"candidate-v2").unwrap();
        let changed = ImportStore::new(store.path())
            .import(&PathWorkspaceImporter {
                environment: source.path(),
                harness: harness.path(),
                candidate: &candidate,
                verifier: &verifier,
            })
            .unwrap();
        assert_ne!(first.digest(), changed.digest());
    }

    fn make_task(root: &Path, prompt: &str) {
        fs::create_dir(root.join("environment")).unwrap();
        fs::create_dir(root.join("tests")).unwrap();
        fs::write(root.join("instruction.md"), prompt).unwrap();
        fs::write(
            root.join("task.toml"),
            r#"schema_version = "1.3"

[task]
name = "case-1"

[agent]
timeout_sec = 30

[verifier]
timeout_sec = 30

[environment]
docker_image = "debian:bookworm-slim"
cpus = 1
memory_mb = 512
storage_mb = 1024
"#,
        )
        .unwrap();
        fs::write(
            root.join("tests/test.sh"),
            "#!/bin/sh\nprintf '1\\n' > \"$NANOCODEX_EVAL_VERIFIER_LOGS/reward.txt\"\n",
        )
        .unwrap();
    }
}
