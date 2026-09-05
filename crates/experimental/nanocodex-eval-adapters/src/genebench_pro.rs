use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use nanocodex_eval::{
    Resources, TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::sha256_values;

/// OpenAI's public GeneBench-Pro case-study package.
#[derive(Clone, Debug)]
pub struct GeneBenchPro {
    package: PathBuf,
    revision: String,
    environment: Environment,
    harness: Harness,
}

impl GeneBenchPro {
    /// Creates an importer for one pinned public package checkout.
    #[must_use]
    pub fn new(
        package: impl Into<PathBuf>,
        revision: impl Into<String>,
        environment: Environment,
        harness: Harness,
    ) -> Self {
        Self {
            package: package.into(),
            revision: revision.into(),
            environment,
            harness,
        }
    }
}

impl DatasetImporter for GeneBenchPro {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let package = canonical_directory(&self.package)?;
        let manifest_path = package.join("manifest.json");
        let manifest_bytes = read_file(&manifest_path)?;
        let manifest: PackageManifest = decode_json(&manifest_path, &manifest_bytes)?;
        if manifest.problem_count != manifest.problems.len() {
            return Err(ImportError::Invalid(format!(
                "GeneBench-Pro manifest declares {} problems but contains {}",
                manifest.problem_count,
                manifest.problems.len()
            )));
        }
        let grader_path = package.join("reference_grader.py");
        let grader = read_file(&grader_path)?;
        let mut source_values = vec![manifest_bytes, grader.clone()];
        let mut cases = Vec::with_capacity(manifest.problems.len());
        for problem in manifest.problems {
            let config_path = resolve_package_file(&package, Path::new(&problem.eval_config))?;
            let config_bytes =
                read_verified_file(&config_path, problem.file(&problem.eval_config)?)?;
            let config: EvalConfig = decode_json(&config_path, &config_bytes)?;
            if config.id != problem.eval_id {
                return Err(ImportError::Invalid(format!(
                    "GeneBench-Pro problem {:?} contains eval config {:?}",
                    problem.eval_id, config.id
                )));
            }
            let problem_root = config_path.parent().ok_or_else(|| {
                ImportError::Invalid(format!(
                    "{} has no problem directory",
                    config_path.display()
                ))
            })?;
            let mut planned = CasePlan::hermetic(
                &problem.eval_id,
                config.task,
                self.environment.clone(),
                self.harness.clone(),
            )?
            .output(TaskOutput::FinalMessage)
            .resources(Resources {
                cpus: 4,
                memory_mb: 8_192,
                storage_mb: 12_288,
                gpus: 0,
            })
            .timeouts(Duration::from_secs(1_800), Duration::from_secs(300))
            // Guest CLI harnesses own their model transport inside the VM.
            // This public case-study package publishes its answers and is not
            // a hidden-answer leaderboard, so its operational recipe permits
            // the network device required by those harnesses.
            .allow_internet(true)
            .harness_file("eval_config.json", config_bytes.clone(), 0o600)?
            .harness_file("reference_grader.py", grader.clone(), 0o700)?;
            source_values.push(config_bytes);
            for relative in config.data_files {
                validate_relative(&relative, "GeneBench-Pro data file")?;
                let package_relative = problem_root
                    .strip_prefix(&package)
                    .map_err(|error| ImportError::Invalid(error.to_string()))?
                    .join(&relative);
                let package_path = resolve_package_file(&package, &package_relative)?;
                let manifest_path = package_relative.to_string_lossy().replace('\\', "/");
                let bytes = read_verified_file(&package_path, problem.file(&manifest_path)?)?;
                planned = planned.environment_file(&relative, bytes.clone(), 0o644)?;
                source_values.push(bytes);
            }
            cases.push(planned);
        }
        let source = SourceIdentity::new(
            "openai-genebench-pro-public-package",
            &self.revision,
            sha256_values(source_values),
        )?;
        let mut plan = DatasetPlan::new("genebench-pro-public", source)?;
        for case in cases {
            plan = plan.case(case);
        }
        Ok(plan)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PackageManifest {
    pub(crate) problem_count: usize,
    pub(crate) problems: Vec<PackageProblem>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PackageProblem {
    pub(crate) eval_id: String,
    pub(crate) eval_config: String,
    files: Vec<PackageFile>,
}

impl PackageProblem {
    fn file(&self, path: &str) -> Result<&PackageFile, ImportError> {
        self.files
            .iter()
            .find(|file| file.path == path)
            .ok_or_else(|| {
                ImportError::Invalid(format!(
                    "GeneBench-Pro manifest problem {:?} omits {path:?}",
                    self.eval_id
                ))
            })
    }

    pub(crate) fn execution_files(&self) -> impl Iterator<Item = &PackageFile> {
        self.files
            .iter()
            .filter(|file| !file.path.ends_with("report_public.pdf"))
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PackageFile {
    pub(crate) path: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Debug, Deserialize)]
struct EvalConfig {
    id: String,
    task: String,
    data_files: Vec<PathBuf>,
    #[serde(flatten)]
    _grader_contract: BTreeMap<String, serde_json::Value>,
}

pub(crate) fn decode_manifest(path: &Path, bytes: &[u8]) -> Result<PackageManifest, String> {
    serde_json::from_slice(bytes)
        .map_err(|error| format!("failed to decode {}: {error}", path.display()))
}

fn decode_json<T: for<'de> Deserialize<'de>>(path: &Path, bytes: &[u8]) -> Result<T, ImportError> {
    serde_json::from_slice(bytes).map_err(|source| {
        ImportError::Invalid(format!("failed to decode {}: {source}", path.display()))
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ImportError> {
    let canonical = fs::canonicalize(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(ImportError::Invalid(format!(
            "GeneBench-Pro package is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn resolve_package_file(package: &Path, relative: &Path) -> Result<PathBuf, ImportError> {
    validate_relative(relative, "GeneBench-Pro package file")?;
    let path = package.join(relative);
    let canonical = fs::canonicalize(&path).map_err(|source| ImportError::Io {
        path: path.clone(),
        source,
    })?;
    if !canonical.starts_with(package) || !canonical.is_file() {
        return Err(ImportError::Invalid(format!(
            "GeneBench-Pro package file escapes its package: {}",
            relative.display()
        )));
    }
    Ok(canonical)
}

fn validate_relative(path: &Path, label: &str) -> Result<(), ImportError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ImportError::Invalid(format!(
            "{label} is not a safe relative path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn read_verified_file(path: &Path, expected: &PackageFile) -> Result<Vec<u8>, ImportError> {
    let bytes = read_file(path)?;
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let digest = hex::encode(Sha256::digest(&bytes));
    if actual_len != expected.bytes || digest != expected.sha256 {
        return Err(ImportError::Invalid(format!(
            "GeneBench-Pro package file does not match manifest: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

fn read_file(path: &Path) -> Result<Vec<u8>, ImportError> {
    fs::read(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use nanocodex_eval::{NetworkPolicy, import::ImportStore};

    use super::*;

    #[test]
    fn exposes_only_declared_data_to_the_candidate() {
        let package = tempfile::tempdir().unwrap();
        let problem = package.path().join("problems/case-1");
        fs::create_dir_all(problem.join("data_files")).unwrap();
        let config = br#"{
  "id": "case-1",
  "task": "Analyze the supplied data and return JSON.",
  "data_files": ["data_files/input.tsv.gz"],
  "ground_truth": {"value": 2},
  "grader": {"type": "numeric_tolerance", "config": {"key": "value"}}
}"#;
        let data = b"compressed-fixture";
        let grader = b"# official grader\n";
        fs::write(problem.join("eval_config.json"), config).unwrap();
        fs::write(problem.join("data_files/input.tsv.gz"), data).unwrap();
        fs::write(package.path().join("reference_grader.py"), grader).unwrap();
        let descriptor = |path: &str, bytes: &[u8]| {
            serde_json::json!({
                "path": path,
                "bytes": bytes.len(),
                "sha256": hex::encode(Sha256::digest(bytes)),
            })
        };
        fs::write(
            package.path().join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "problem_count": 1,
                "problems": [{
                    "eval_id": "case-1",
                    "eval_config": "problems/case-1/eval_config.json",
                    "files": [
                        descriptor("problems/case-1/eval_config.json", config),
                        descriptor("problems/case-1/data_files/input.tsv.gz", data),
                    ],
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/genebench-pro");
        let store = tempfile::tempdir().unwrap();

        let imported = ImportStore::new(store.path())
            .import(&GeneBenchPro::new(
                package.path(),
                "openai/genebench@fixture",
                Environment::Dockerfile(assets.join("environment")),
                Harness::directory(assets.join("verifier")).unwrap(),
            ))
            .unwrap();
        let task = &imported.tasks()[0];

        assert_eq!(task.output(), TaskOutput::FinalMessage);
        assert_eq!(task.network(), NetworkPolicy::Public);
        assert_eq!(
            fs::read(task.root().join("environment/data_files/input.tsv.gz")).unwrap(),
            data
        );
        assert!(task.root().join("tests/eval_config.json").is_file());
        assert!(!task.root().join("environment/eval_config.json").exists());
    }
}
