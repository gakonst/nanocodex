//! Closed evaluation profiles over native Terminal-Bench task packages.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use nanocodex_oai_api::{Model, Thinking};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{Task, TaskLoadError};

const BUILTIN_HARNESS: &str = "nanocodex";

/// Repository-level native evaluation configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationManifest {
    default: Option<String>,
    #[serde(default)]
    harness: BTreeMap<String, Harness>,
    #[serde(default, rename = "benchmark")]
    _benchmarks: BTreeMap<String, toml::Value>,
    profiles: BTreeMap<String, Profile>,
}

/// One pinned external evaluation harness.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    command: PathBuf,
    /// Absolute executable path installed in every prepared task image.
    guest_command: String,
    /// Semantic harness release shared by architecture-specific executables.
    ///
    /// When omitted, legacy manifests continue to pin the exact command bytes.
    version: Option<String>,
    /// Complete guest argument template for the harness JSONL contract.
    arguments: Vec<String>,
    /// Harness-specific guest environment variables.
    #[serde(default)]
    environment: BTreeMap<String, String>,
    /// Guest home exposed to the harness and available as a template value.
    #[serde(default = "default_harness_home")]
    home: String,
    /// Guest destination for file-based credentials.
    #[serde(default = "default_harness_auth_file")]
    auth_file: String,
    /// Guest environment variable receiving API-key credentials.
    #[serde(default = "default_harness_api_key_environment")]
    api_key_environment: String,
    /// OpenAI-compatible upstream reached through the capture proxy.
    api_upstream: Option<String>,
}

/// One closed desired bundle of native task coordinates.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Adapter-owned benchmark selectors resolved before durable work is added.
    #[serde(default)]
    benchmarks: Vec<String>,
    #[serde(default)]
    tasks: Vec<PathBuf>,
    #[serde(default)]
    suites: Vec<PathBuf>,
    trials: u16,
    #[serde(default = "default_harnesses")]
    harness: Vec<String>,
    #[serde(default = "default_models")]
    model: Vec<Model>,
    #[serde(
        default = "default_thinking",
        deserialize_with = "deserialize_thinking",
        serialize_with = "serialize_thinking"
    )]
    thinking: Vec<Thinking>,
    #[serde(default)]
    web_search: bool,
}

/// Parsed and content-pinned profile revision.
#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    /// Selected profile name.
    #[cfg(test)]
    pub name: String,
    /// Stable digest of all resolved profile inputs.
    #[cfg(test)]
    pub digest: String,
    /// Loaded immutable task packages.
    pub tasks: Vec<ResolvedTask>,
    /// Exact task/treatment families, excluding fungible repetitions.
    pub families: Vec<ResolvedFamily>,
    /// Whether model-facing web search is enabled.
    pub web_search: bool,
    /// Number of desired repetitions for every family.
    pub trials: u16,
}

/// One profile-visible selector bound to a loaded task package.
#[derive(Clone, Debug)]
pub struct ResolvedTask {
    /// Exact selector accepted by `nanocodex eval run --task`.
    pub selector: String,
    /// Loaded immutable task package.
    pub task: Task,
}

/// One content-pinned external harness selected by a profile.
#[derive(Clone, Debug)]
pub struct ResolvedHarness {
    /// Profile-visible harness name.
    pub name: String,
    /// Architecture-local executable.
    pub command: PathBuf,
    /// Executable path inside the prepared task image.
    pub guest_command: String,
    /// Guest argument template.
    pub arguments: Vec<String>,
    /// Guest environment additions.
    pub environment: BTreeMap<String, String>,
    /// Generic writable home staged for this harness.
    pub home: String,
    /// Guest destination for file-based credentials.
    pub auth_file: String,
    /// Guest environment variable receiving API-key credentials.
    pub api_key_environment: String,
    /// Capture-proxy upstream override.
    pub api_upstream: Option<String>,
    /// Semantic version retained in evidence.
    pub version: String,
}

/// One exact semantic treatment family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedFamily {
    /// Stable identity excluding fungible repetition.
    pub key: String,
    /// Task selector owned by this family.
    pub task: String,
    /// Built-in or configured harness used for this coordinate.
    pub harness: String,
    /// Supported model selection.
    pub model: Model,
    /// Reasoning effort.
    #[serde(serialize_with = "serialize_one_thinking")]
    pub thinking: Thinking,
}

/// Profile parsing or resolution failure.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// Manifest could not be read.
    #[error("failed to read evaluation manifest {path}: {source}")]
    Read {
        /// Requested manifest path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Manifest TOML was invalid.
    #[error("failed to parse evaluation manifest {path}: {source}")]
    Parse {
        /// Requested manifest path.
        path: PathBuf,
        /// TOML decoding failure.
        source: toml::de::Error,
    },
    /// Neither an explicit profile nor a manifest default was available.
    #[error("evaluation profile is required because the manifest has no default")]
    MissingProfile,
    /// The requested profile was absent.
    #[error("evaluation profile `{0}` does not exist")]
    UnknownProfile(String),
    /// Profile had no task inputs.
    #[error("evaluation profile `{0}` contains no benchmarks, tasks, or suites")]
    EmptyProfile(String),
    /// Adapter-backed benchmark selectors were not resolved to immutable tasks.
    #[error("evaluation profile `{profile}` requires adapter resolution for: {benchmarks}")]
    UnresolvedBenchmarks {
        /// Selected profile.
        profile: String,
        /// Adapter-owned selectors requiring resolution.
        benchmarks: String,
    },
    /// Profile requested no repetitions.
    #[error("evaluation profile `{0}` must request at least one trial")]
    ZeroTrials(String),
    /// Profile treatment matrix had an empty dimension.
    #[error("evaluation profile `{profile}` has no {dimension} values")]
    EmptyDimension {
        /// Invalid profile.
        profile: String,
        /// Empty semantic dimension.
        dimension: &'static str,
    },
    /// A profile repeated one harness name.
    #[error("evaluation profile `{profile}` contains duplicate harness `{harness}`")]
    DuplicateHarness {
        /// Invalid profile.
        profile: String,
        /// Repeated harness name.
        harness: String,
    },
    /// A suite had no immediate task children.
    #[error("suite contains no immediate task directories: {0}")]
    EmptySuite(PathBuf),
    /// Two task inputs resolved to the same selector.
    #[error("profile contains duplicate task selector `{0}`")]
    DuplicateTask(String),
    /// Two task inputs resolved to the same canonical package.
    #[error("profile contains the task package more than once: {0}")]
    DuplicateTaskRoot(PathBuf),
    /// A task package failed to load.
    #[error(transparent)]
    Task(#[from] TaskLoadError),
    /// A profile selected an external harness without configuring it.
    #[error("evaluation profile `{profile}` selects unknown harness `{harness}`")]
    UnknownHarness {
        /// Invalid profile.
        profile: String,
        /// Missing top-level harness configuration.
        harness: String,
    },
    /// A semantic harness version was present but empty.
    #[error("evaluation harness `{0}` has an empty version")]
    EmptyHarnessVersion(String),
    /// A harness guest command was not an absolute path.
    #[error("evaluation harness `{harness}` guest_command must be a clean absolute path: {path}")]
    InvalidHarnessGuestCommand {
        /// Invalid harness name.
        harness: String,
        /// Rejected guest path.
        path: String,
    },
    /// A resolved path could not be canonicalized.
    #[error("failed to resolve {path}: {source}")]
    ResolvePath {
        /// Path being resolved.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// A pinned harness executable could not be fingerprinted.
    #[error("failed to fingerprint evaluation harness {path}: {source}")]
    FingerprintHarness {
        /// Harness executable being fingerprinted.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Stable identity serialization failed.
    #[error("failed to serialize resolved profile identity: {0}")]
    Identity(#[from] serde_json::Error),
}

/// Closed-profile selector failure.
#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProfileSelectionError {
    /// Requested task is outside the profile.
    #[error("task `{selector}` is not part of profile `{profile}`")]
    Task {
        /// Selected profile.
        profile: String,
        /// Rejected selector.
        selector: String,
    },
    /// No treatment matched explicit selectors.
    #[error("no treatment in profile `{profile}` matches task `{task}` and the requested knobs")]
    Treatment {
        /// Selected profile.
        profile: String,
        /// Selected task.
        task: String,
    },
    /// Omitted semantic knobs did not identify one family.
    #[error(
        "task `{task}` has multiple treatments in profile `{profile}`; select model and/or thinking"
    )]
    Ambiguous {
        /// Selected profile.
        profile: String,
        /// Selected task.
        task: String,
    },
}

#[derive(Serialize)]
struct ProfileIdentity<'a> {
    schema: u32,
    name: &'a str,
    profile: &'a Profile,
    tasks: Vec<TaskIdentity<'a>>,
    harness_digests: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct TaskIdentity<'a> {
    selector: &'a str,
    digest: &'a str,
}

impl EvaluationManifest {
    /// Loads a manifest and resolves one immutable profile revision.
    pub fn load_profile(
        path: impl AsRef<Path>,
        requested: Option<&str>,
    ) -> Result<ResolvedProfile, ProfileError> {
        Self::load_profile_with_tasks(path, requested, Vec::new())
    }

    /// Loads a manifest after adapters resolved its benchmark selectors.
    pub fn load_profile_with_tasks(
        path: impl AsRef<Path>,
        requested: Option<&str>,
        benchmark_tasks: Vec<ResolvedTask>,
    ) -> Result<ResolvedProfile, ProfileError> {
        let requested_path = path.as_ref();
        let text = fs::read_to_string(requested_path).map_err(|source| ProfileError::Read {
            path: requested_path.to_path_buf(),
            source,
        })?;
        let manifest: Self = toml::from_str(&text).map_err(|source| ProfileError::Parse {
            path: requested_path.to_path_buf(),
            source,
        })?;
        let config_path =
            requested_path
                .canonicalize()
                .map_err(|source| ProfileError::ResolvePath {
                    path: requested_path.to_path_buf(),
                    source,
                })?;
        manifest.resolve(config_path, requested, benchmark_tasks)
    }

    /// Reads the adapter-owned benchmark selectors requested by one profile.
    pub fn load_benchmarks(
        path: impl AsRef<Path>,
        requested: Option<&str>,
    ) -> Result<Vec<String>, ProfileError> {
        let requested_path = path.as_ref();
        let text = fs::read_to_string(requested_path).map_err(|source| ProfileError::Read {
            path: requested_path.to_path_buf(),
            source,
        })?;
        let manifest: Self = toml::from_str(&text).map_err(|source| ProfileError::Parse {
            path: requested_path.to_path_buf(),
            source,
        })?;
        let name = requested
            .map(ToOwned::to_owned)
            .or_else(|| manifest.default.clone())
            .ok_or(ProfileError::MissingProfile)?;
        let profile = manifest
            .profiles
            .get(&name)
            .ok_or_else(|| ProfileError::UnknownProfile(name.clone()))?;
        validate_profile(&name, profile)?;
        Ok(profile.benchmarks.clone())
    }

    /// Resolves one named runtime harness helper from current local config.
    pub fn load_harness(
        path: impl AsRef<Path>,
        name: &str,
    ) -> Result<Option<ResolvedHarness>, ProfileError> {
        if name == BUILTIN_HARNESS {
            return Ok(None);
        }
        let requested_path = path.as_ref();
        let text = fs::read_to_string(requested_path).map_err(|source| ProfileError::Read {
            path: requested_path.to_path_buf(),
            source,
        })?;
        let manifest: Self = toml::from_str(&text).map_err(|source| ProfileError::Parse {
            path: requested_path.to_path_buf(),
            source,
        })?;
        let config_path =
            requested_path
                .canonicalize()
                .map_err(|source| ProfileError::ResolvePath {
                    path: requested_path.to_path_buf(),
                    source,
                })?;
        let harness = manifest
            .harness
            .get(name)
            .ok_or_else(|| ProfileError::UnknownHarness {
                profile: "runtime".to_owned(),
                harness: name.to_owned(),
            })?;
        if !Path::new(&harness.guest_command).is_absolute()
            || harness.guest_command.chars().any(char::is_whitespace)
        {
            return Err(ProfileError::InvalidHarnessGuestCommand {
                harness: name.to_owned(),
                path: harness.guest_command.clone(),
            });
        }
        let root = config_path
            .parent()
            .expect("a canonical config path has a parent");
        let command = resolve_path(root, &harness.command)?;
        let version = match harness.version.as_deref() {
            Some(version) if version.trim().is_empty() => {
                return Err(ProfileError::EmptyHarnessVersion(name.to_owned()));
            }
            Some(version) => version.to_owned(),
            None => harness_digest(&command)?,
        };
        Ok(Some(ResolvedHarness {
            name: name.to_owned(),
            command,
            guest_command: harness.guest_command.clone(),
            arguments: harness.arguments.clone(),
            environment: harness.environment.clone(),
            home: harness.home.clone(),
            auth_file: harness.auth_file.clone(),
            api_key_environment: harness.api_key_environment.clone(),
            api_upstream: harness.api_upstream.clone(),
            version,
        }))
    }

    fn resolve(
        self,
        config_path: PathBuf,
        requested: Option<&str>,
        benchmark_tasks: Vec<ResolvedTask>,
    ) -> Result<ResolvedProfile, ProfileError> {
        let name = requested
            .map(ToOwned::to_owned)
            .or_else(|| self.default.clone())
            .ok_or(ProfileError::MissingProfile)?;
        let profile = self
            .profiles
            .get(&name)
            .ok_or_else(|| ProfileError::UnknownProfile(name.clone()))?;
        validate_profile(&name, profile)?;
        if !profile.benchmarks.is_empty() && benchmark_tasks.is_empty() {
            return Err(ProfileError::UnresolvedBenchmarks {
                profile: name.clone(),
                benchmarks: profile.benchmarks.join(", "),
            });
        }
        let root = config_path
            .parent()
            .expect("a canonical manifest path has a parent");
        let tasks = load_tasks(root, profile, benchmark_tasks)?;
        let mut harness_digests = BTreeMap::new();
        for harness_name in &profile.harness {
            if harness_name == BUILTIN_HARNESS {
                continue;
            }
            let harness =
                self.harness
                    .get(harness_name)
                    .ok_or_else(|| ProfileError::UnknownHarness {
                        profile: name.clone(),
                        harness: harness_name.clone(),
                    })?;
            if !Path::new(&harness.guest_command).is_absolute()
                || harness.guest_command.chars().any(char::is_whitespace)
                || !Path::new(&harness.guest_command)
                    .components()
                    .all(|component| {
                        matches!(
                            component,
                            std::path::Component::RootDir | std::path::Component::Normal(_)
                        )
                    })
            {
                return Err(ProfileError::InvalidHarnessGuestCommand {
                    harness: harness_name.clone(),
                    path: harness.guest_command.clone(),
                });
            }
            let command = resolve_path(root, &harness.command)?;
            let command_identity = match harness.version.as_deref() {
                Some(version) if version.trim().is_empty() => {
                    return Err(ProfileError::EmptyHarnessVersion(harness_name.clone()));
                }
                Some(version) => format!("version:{version}"),
                None => harness_digest(&command)?,
            };
            let digest = hex::encode(Sha256::digest(serde_json::to_vec(&(
                harness,
                &command_identity,
            ))?));
            harness_digests.insert(harness_name.clone(), digest);
        }
        let families = expand_families(profile, &tasks);
        let identity = ProfileIdentity {
            schema: 3,
            name: &name,
            profile,
            tasks: tasks
                .iter()
                .map(|task| TaskIdentity {
                    selector: &task.selector,
                    digest: task.task.package_digest(),
                })
                .collect(),
            harness_digests: &harness_digests,
        };
        #[cfg(test)]
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&identity)?));
        #[cfg(not(test))]
        let _ = identity;
        Ok(ResolvedProfile {
            #[cfg(test)]
            name,
            #[cfg(test)]
            digest,
            tasks,
            families,
            web_search: profile.web_search,
            trials: profile.trials,
        })
    }
}

impl ResolvedProfile {
    /// Resolves one exact task selector without permitting ad-hoc expansion.
    #[cfg(test)]
    pub fn task(&self, selector: &str) -> Result<&ResolvedTask, ProfileSelectionError> {
        self.tasks
            .iter()
            .find(|task| task.selector == selector)
            .ok_or_else(|| ProfileSelectionError::Task {
                profile: self.name.clone(),
                selector: selector.to_owned(),
            })
    }

    /// Resolves an exact task family. Omitted dimensions are accepted only
    /// when the profile contains one unambiguous matching treatment.
    #[cfg(test)]
    pub fn family(
        &self,
        task: &str,
        harness: Option<&str>,
        model: Option<Model>,
        thinking: Option<Thinking>,
    ) -> Result<&ResolvedFamily, ProfileSelectionError> {
        self.task(task)?;
        let harness = harness.unwrap_or(BUILTIN_HARNESS);
        let matching = self
            .families
            .iter()
            .filter(|family| {
                family.task == task
                    && family.harness == harness
                    && model.is_none_or(|model| family.model == model)
                    && thinking.is_none_or(|thinking| family.thinking == thinking)
            })
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [family] => Ok(family),
            [] => Err(ProfileSelectionError::Treatment {
                profile: self.name.clone(),
                task: task.to_owned(),
            }),
            _ => Err(ProfileSelectionError::Ambiguous {
                profile: self.name.clone(),
                task: task.to_owned(),
            }),
        }
    }
}

fn validate_profile(name: &str, profile: &Profile) -> Result<(), ProfileError> {
    if profile.trials == 0 {
        return Err(ProfileError::ZeroTrials(name.to_owned()));
    }
    if profile.benchmarks.is_empty() && profile.tasks.is_empty() && profile.suites.is_empty() {
        return Err(ProfileError::EmptyProfile(name.to_owned()));
    }
    for (values, dimension) in [
        (profile.harness.len(), "harness"),
        (profile.model.len(), "model"),
        (profile.thinking.len(), "thinking"),
    ] {
        if values == 0 {
            return Err(ProfileError::EmptyDimension {
                profile: name.to_owned(),
                dimension,
            });
        }
    }
    let mut harnesses = BTreeSet::new();
    for harness in &profile.harness {
        if !harnesses.insert(harness) {
            return Err(ProfileError::DuplicateHarness {
                profile: name.to_owned(),
                harness: harness.clone(),
            });
        }
    }
    Ok(())
}

fn load_tasks(
    root: &Path,
    profile: &Profile,
    benchmark_tasks: Vec<ResolvedTask>,
) -> Result<Vec<ResolvedTask>, ProfileError> {
    let mut inputs = profile
        .tasks
        .iter()
        .map(|path| {
            (
                path.to_string_lossy().into_owned(),
                resolve_path(root, path),
            )
        })
        .map(|(selector, path)| path.map(|path| (selector, path)))
        .collect::<Result<Vec<_>, _>>()?;
    for suite in &profile.suites {
        let suite_root = resolve_path(root, suite)?;
        let entries = fs::read_dir(&suite_root).map_err(|source| ProfileError::Read {
            path: suite_root.clone(),
            source,
        })?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ProfileError::Read {
                path: suite_root.clone(),
                source,
            })?;
            let kind = entry.file_type().map_err(|source| ProfileError::Read {
                path: entry.path(),
                source,
            })?;
            let path = entry.path();
            if kind.is_dir() && path.join("task.toml").is_file() {
                children.push(path);
            }
        }
        children.sort();
        if children.is_empty() {
            return Err(ProfileError::EmptySuite(suite_root));
        }
        let prefix = suite.to_string_lossy();
        inputs.extend(children.into_iter().map(|path| {
            let name = path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            (format!("{prefix}/{name}"), path)
        }));
    }
    let mut selectors = BTreeSet::new();
    let mut roots = BTreeSet::new();
    let mut tasks = inputs
        .into_iter()
        .map(|(selector, path)| {
            if !selectors.insert(selector.clone()) {
                return Err(ProfileError::DuplicateTask(selector));
            }
            if !roots.insert(path.clone()) {
                return Err(ProfileError::DuplicateTaskRoot(path));
            }
            Ok(ResolvedTask {
                selector,
                task: Task::load(path)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for task in benchmark_tasks {
        if !selectors.insert(task.selector.clone()) {
            return Err(ProfileError::DuplicateTask(task.selector));
        }
        let root = task.task.root().to_path_buf();
        if !roots.insert(root.clone()) {
            return Err(ProfileError::DuplicateTaskRoot(root));
        }
        tasks.push(task);
    }
    Ok(tasks)
}

fn expand_families(profile: &Profile, tasks: &[ResolvedTask]) -> Vec<ResolvedFamily> {
    let mut families = Vec::new();
    for task in tasks {
        for harness in &profile.harness {
            for model in &profile.model {
                for thinking in &profile.thinking {
                    let key = format!(
                        "{}|{}|{}|{}",
                        task.selector,
                        harness,
                        model.as_str(),
                        thinking.as_str(),
                    );
                    families.push(ResolvedFamily {
                        key,
                        task: task.selector.clone(),
                        harness: harness.clone(),
                        model: *model,
                        thinking: *thinking,
                    });
                }
            }
        }
    }
    families
}

fn resolve_path(root: &Path, path: &Path) -> Result<PathBuf, ProfileError> {
    let requested = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    requested
        .canonicalize()
        .map_err(|source| ProfileError::ResolvePath {
            path: requested,
            source,
        })
}

fn harness_digest(path: &Path) -> Result<String, ProfileError> {
    let mut file = fs::File::open(path).map_err(|source| ProfileError::FingerprintHarness {
        path: path.to_path_buf(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| ProfileError::FingerprintHarness {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn default_models() -> Vec<Model> {
    vec![Model::default()]
}

fn default_harnesses() -> Vec<String> {
    vec![BUILTIN_HARNESS.to_owned()]
}

fn default_harness_home() -> String {
    "/run/nanocodex-harness-home".to_owned()
}

fn default_harness_auth_file() -> String {
    "/run/nanocodex-harness-home/auth.json".to_owned()
}

fn default_harness_api_key_environment() -> String {
    "OPENAI_API_KEY".to_owned()
}

fn default_thinking() -> Vec<Thinking> {
    vec![Thinking::default()]
}

fn deserialize_thinking<'de, D>(deserializer: D) -> Result<Vec<Thinking>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| value.parse().map_err(serde::de::Error::custom))
        .collect()
}

fn serialize_thinking<S>(values: &[Thinking], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    values
        .iter()
        .map(|value| value.as_str())
        .collect::<Vec<_>>()
        .serialize(serializer)
}

pub(crate) fn serialize_one_thinking<S>(value: &Thinking, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_task(root: &Path, name: &str) {
        let task = root.join(name);
        fs::create_dir_all(task.join("environment")).unwrap();
        fs::create_dir_all(task.join("tests")).unwrap();
        fs::write(
            task.join("task.toml"),
            format!(
                r#"schema_version = "1.1"
[task]
name = "{name}"
description = "test"
[agent]
timeout_sec = 1.0
[verifier]
timeout_sec = 1.0
[environment]
docker_image = "alpine:3.21"
cpus = 1
memory_mb = 128
storage_mb = 128
gpus = 0
allow_internet = false
"#
            ),
        )
        .unwrap();
        fs::write(task.join("instruction.md"), "do it").unwrap();
        fs::write(task.join("environment/Dockerfile"), "FROM scratch").unwrap();
        fs::write(task.join("tests/test.sh"), "#!/bin/sh\n").unwrap();
    }

    #[test]
    fn profile_expands_trials_in_sqlite_but_not_as_agent_selectors() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path(), "one");
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"default = "release"
[profiles.release]
tasks = ["one"]
trials = 3
model = ["sol"]
thinking = ["high"]
"#,
        )
        .unwrap();

        let profile = EvaluationManifest::load_profile(&config, None).unwrap();
        assert_eq!(profile.name, "release");
        assert_eq!(profile.families.len(), 1);
        assert_eq!(profile.trials, 3);
        assert_eq!(profile.task("one").unwrap().task.name(), "one");
    }

    #[test]
    fn adapter_tasks_resolve_declared_benchmark_selectors() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path(), "imported");
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"default = "smoke"
[profiles.smoke]
benchmarks = ["terminal-bench-2.1/fix-git"]
trials = 1
"#,
        )
        .unwrap();

        assert_eq!(
            EvaluationManifest::load_benchmarks(&config, None).unwrap(),
            ["terminal-bench-2.1/fix-git"]
        );
        assert!(matches!(
            EvaluationManifest::load_profile(&config, None),
            Err(ProfileError::UnresolvedBenchmarks { .. })
        ));
        let profile = EvaluationManifest::load_profile_with_tasks(
            &config,
            None,
            vec![ResolvedTask {
                selector: "terminal-bench-2.1/fix-git".to_owned(),
                task: Task::load(directory.path().join("imported")).unwrap(),
            }],
        )
        .unwrap();

        assert_eq!(profile.tasks.len(), 1);
        assert_eq!(profile.tasks[0].selector, "terminal-bench-2.1/fix-git");
    }

    #[test]
    fn task_selector_cannot_expand_the_profile() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path(), "included");
        write_task(directory.path(), "outside");
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"[profiles.release]
tasks = ["included"]
trials = 1
"#,
        )
        .unwrap();
        let profile = EvaluationManifest::load_profile(&config, Some("release")).unwrap();

        assert!(matches!(
            profile.task("outside"),
            Err(ProfileSelectionError::Task { selector, .. }) if selector == "outside"
        ));
    }

    #[test]
    fn external_harness_revision_pins_the_command_bytes() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path(), "one");
        let codex = directory.path().join("codex");
        fs::write(&codex, "first build").unwrap();
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"[harness.codex]
command = "codex"
guest_command = "/usr/local/bin/codex"
arguments = ["{prompt}"]

[profiles.release]
tasks = ["one"]
trials = 1
harness = ["nanocodex", "codex"]
"#,
        )
        .unwrap();

        let first = EvaluationManifest::load_profile(&config, Some("release")).unwrap();
        assert_eq!(first.families.len(), 2);
        assert_eq!(
            first.family("one", None, None, None).unwrap().harness,
            "nanocodex"
        );
        assert_eq!(
            first
                .family("one", Some("codex"), None, None)
                .unwrap()
                .harness,
            "codex"
        );
        fs::write(&codex, "second build").unwrap();
        let second = EvaluationManifest::load_profile(&config, Some("release")).unwrap();

        assert_ne!(first.digest, second.digest);
    }

    #[test]
    fn external_harness_can_pin_one_version_across_architecture_builds() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (directory, command) in [(&first, "aarch64 build"), (&second, "x86_64 build")] {
            write_task(directory.path(), "one");
            fs::write(directory.path().join("codex"), command).unwrap();
            fs::write(
                directory.path().join("nanocodex.toml"),
                r#"[harness.codex]
command = "codex"
guest_command = "/usr/local/bin/codex"
version = "0.145.0"
arguments = ["{prompt}"]

[profiles.release]
tasks = ["one"]
trials = 1
harness = ["nanocodex", "codex"]
"#,
            )
            .unwrap();
        }

        let first =
            EvaluationManifest::load_profile(first.path().join("nanocodex.toml"), Some("release"))
                .unwrap();
        let second =
            EvaluationManifest::load_profile(second.path().join("nanocodex.toml"), Some("release"))
                .unwrap();

        assert_eq!(first.digest, second.digest);
    }

    #[test]
    fn profile_revision_is_independent_of_the_checkout_path() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for directory in [&first, &second] {
            write_task(directory.path(), "one");
            fs::write(
                directory.path().join("nanocodex.toml"),
                r#"[profiles.release]
tasks = ["one"]
trials = 2
model = ["sol"]
thinking = ["high"]
"#,
            )
            .unwrap();
        }

        let first =
            EvaluationManifest::load_profile(first.path().join("nanocodex.toml"), Some("release"))
                .unwrap();
        let second =
            EvaluationManifest::load_profile(second.path().join("nanocodex.toml"), Some("release"))
                .unwrap();

        assert_eq!(first.digest, second.digest);
    }
}
