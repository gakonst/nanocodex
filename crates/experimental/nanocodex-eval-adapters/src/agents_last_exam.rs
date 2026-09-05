use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

use nanocodex_eval::{
    Resources, ScoringPolicy,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};
use serde::Deserialize;

use crate::{sha256_file, sha256_values};

const TASKS: [AleTask; 4] = [
    AleTask::new("computing_math/branch_bound_atsp", "branch_bound_atsp"),
    AleTask::new(
        "business_finance/basel_operational_risk_bia_cn",
        "basel_operational_risk_bia_cn",
    ),
    AleTask::new(
        "business_finance/financial_stmt_reconstruction_aapl_fy2024",
        "financial_stmt_reconstruction_aapl_fy2024",
    ),
    AleTask::new(
        "social_sciences/atwood_2022_measles_vaccine_reproduction",
        "atwood_2022_measles_vaccine_reproduction",
    ),
];

#[derive(Clone, Copy, Debug)]
struct AleTask {
    source_id: &'static str,
    case_id: &'static str,
}

impl AleTask {
    const fn new(source_id: &'static str, case_id: &'static str) -> Self {
        Self { source_id, case_id }
    }
}

/// Berkeley RDI's public Agents' Last Exam CLI-compatible Linux task package.
#[derive(Clone, Debug)]
pub struct AgentsLastExam {
    source: PathBuf,
    task_data: PathBuf,
    revision: String,
    environment: Environment,
    harness: Harness,
}

impl AgentsLastExam {
    /// Creates an importer from a pinned ALE checkout and its separately staged
    /// gated task-data archive.
    #[must_use]
    pub fn new(
        source: impl Into<PathBuf>,
        task_data: impl Into<PathBuf>,
        revision: impl Into<String>,
        environment: Environment,
        harness: Harness,
    ) -> Self {
        Self {
            source: source.into(),
            task_data: task_data.into(),
            revision: revision.into(),
            environment,
            harness,
        }
    }
}

impl DatasetImporter for AgentsLastExam {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let source = canonical_directory(&self.source, "ALE source checkout")?;
        let mut source_digests = Vec::new();
        let mut cases = Vec::with_capacity(TASKS.len());
        for recipe in TASKS {
            let task_root = source.join("tasks").join(recipe.source_id);
            let task_card_path = task_root.join("task_card.json");
            let task_card_bytes = read_file(&task_card_path)?;
            let task_card: TaskCard =
                serde_json::from_slice(&task_card_bytes).map_err(|error| {
                    ImportError::Invalid(format!(
                        "failed to decode {}: {error}",
                        task_card_path.display()
                    ))
                })?;
            if task_card.task_id != recipe.source_id {
                return Err(ImportError::Invalid(format!(
                    "ALE task card has taskId {:?}, expected {:?}",
                    task_card.task_id, recipe.source_id
                )));
            }
            let timeout = task_card.vm.timeout_seconds()?;
            let scorer = task_root.join("scripts/score_outputs.py");
            let data_root = canonical_directory(
                &self.task_data.join(recipe.source_id).join("base"),
                "ALE selected task data",
            )?;

            source_digests.push(sha256_values([&task_card_bytes]));
            source_digests.push(sha256_file(&scorer)?);
            let mut task = CasePlan::hermetic(
                recipe.case_id,
                task_card.task_prompt,
                self.environment.clone(),
                self.harness.clone(),
            )?
            .benchmark_case_type("ale-cli-linux")
            .resources(Resources {
                cpus: 4,
                memory_mb: 15_360,
                storage_mb: 204_800,
                gpus: 0,
            })
            .timeouts(Duration::from_secs(timeout), Duration::from_secs(900))
            .scoring_policy(ScoringPolicy::AllRewardsOne)
            // ALE's published agent configurations explicitly permit internet
            // access; task-level prompts remain authoritative about prohibited
            // external solvers or replacement inputs.
            .allow_internet(true)
            .harness_file_from("score_outputs.py", &scorer, file_mode(&scorer)?)?;

            for relative in ["input", "software"] {
                let root = data_root.join(relative);
                for file in regular_files(&root)? {
                    let nested = file.strip_prefix(&data_root).map_err(|error| {
                        ImportError::Invalid(format!("failed to relativize ALE task data: {error}"))
                    })?;
                    source_digests.push(sha256_file(&file)?);
                    task = task.environment_file_from(
                        Path::new("base").join(nested),
                        &file,
                        file_mode(&file)?,
                    )?;
                }
            }
            let references = data_root.join("reference");
            for file in regular_files(&references)? {
                let nested = file.strip_prefix(&references).map_err(|error| {
                    ImportError::Invalid(format!(
                        "failed to relativize ALE reference data: {error}"
                    ))
                })?;
                source_digests.push(sha256_file(&file)?);
                task = task.harness_file_from(
                    Path::new("reference").join(nested),
                    &file,
                    file_mode(&file)?,
                )?;
            }
            cases.push(task);
        }

        let source = SourceIdentity::new(
            "agents-last-exam",
            &self.revision,
            sha256_values(source_digests.iter().map(String::as_bytes)),
        )?;
        let mut plan = DatasetPlan::new("agents-last-exam", source)?;
        for case in cases {
            plan = plan.case(case);
        }
        Ok(plan)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskCard {
    task_id: String,
    task_prompt: String,
    vm: TaskVm,
}

#[derive(Debug, Deserialize)]
struct TaskVm {
    timeout: Option<u64>,
    timeout_s: Option<u64>,
}

impl TaskVm {
    fn timeout_seconds(&self) -> Result<u64, ImportError> {
        self.timeout
            .or(self.timeout_s)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                ImportError::Invalid("ALE task card is missing a positive VM timeout".to_owned())
            })
    }
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, ImportError> {
    let canonical = fs::canonicalize(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(ImportError::Invalid(format!(
            "{label} is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn regular_files(root: &Path) -> Result<Vec<PathBuf>, ImportError> {
    let root = canonical_directory(root, "ALE task-data component")?;
    let mut files = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .build()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.path() == root => None,
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => {
                Some(Ok(entry.into_path()))
            }
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_dir()) => None,
            Ok(entry) => Some(Err(ImportError::Invalid(format!(
                "unsupported ALE task-data entry: {}",
                entry.path().display()
            )))),
            Err(error) => Some(Err(ImportError::Invalid(format!(
                "failed to walk ALE task data: {error}"
            )))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_unstable();
    Ok(files)
}

fn read_file(path: &Path) -> Result<Vec<u8>, ImportError> {
    fs::read(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn file_mode(path: &Path) -> Result<u32, ImportError> {
    let mode = fs::metadata(path)
        .map_err(|source| ImportError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions()
        .mode();
    Ok(mode & 0o777)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use nanocodex_eval::{
        NetworkPolicy, ScoringPolicy,
        import::{Environment, Harness, ImportStore},
    };
    use tempfile::tempdir;

    use super::AgentsLastExam;

    #[test]
    fn imports_cli_task_without_exposing_hidden_reference() {
        let fixture = tempdir().unwrap();
        make_fixture(fixture.path());
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/agents-last-exam");
        let store = tempdir().unwrap();
        let dataset = ImportStore::new(store.path())
            .import(&AgentsLastExam::new(
                fixture.path().join("source"),
                fixture.path().join("task-data"),
                "rdi-berkeley/agents-last-exam@abc+data@def",
                Environment::OciImage("ale.example/image@sha256:abc".to_owned()),
                Harness::directory(assets).unwrap(),
            ))
            .unwrap();

        assert_eq!(dataset.tasks().len(), 4);
        for task in dataset.tasks() {
            assert_eq!(task.network(), NetworkPolicy::Public);
            assert_eq!(
                task.verifier().scoring_policy(),
                ScoringPolicy::AllRewardsOne
            );
            assert!(
                task.root()
                    .join("environment/base/input/input.txt")
                    .is_file()
            );
            assert!(
                task.root()
                    .join("environment/base/software/python")
                    .is_file()
            );
            assert!(
                !task
                    .root()
                    .join("environment/base/reference/gold.json")
                    .exists()
            );
            assert!(task.root().join("tests/reference/gold.json").is_file());
            assert!(task.root().join("tests/score_outputs.py").is_file());
        }
    }

    fn make_fixture(root: &Path) {
        for recipe in super::TASKS {
            let task = root.join("source/tasks").join(recipe.source_id);
            fs::create_dir_all(task.join("scripts")).unwrap();
            fs::write(
                task.join("task_card.json"),
                format!(
                    "{{\"taskId\":{task_id:?},\"taskPrompt\":\"Solve it.\",\"vm\":{{\"timeout\":7200}}}}",
                    task_id = recipe.source_id
                ),
            )
            .unwrap();
            fs::write(
                task.join("scripts/score_outputs.py"),
                "def score(output, reference):\n    return {'score': 1.0}\n",
            )
            .unwrap();
            let data = root.join("task-data").join(recipe.source_id).join("base");
            fs::create_dir_all(data.join("input")).unwrap();
            fs::create_dir_all(data.join("software")).unwrap();
            fs::create_dir_all(data.join("reference")).unwrap();
            fs::write(data.join("input/input.txt"), "input").unwrap();
            fs::write(data.join("software/python"), "#!/bin/sh\n").unwrap();
            fs::write(data.join("reference/gold.json"), "{}\n").unwrap();
        }
    }
}
