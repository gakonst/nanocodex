use std::{collections::HashSet, path::PathBuf};

use nanocodex_eval::{
    NetworkPolicy, ScoringPolicy, Task, TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};

use crate::{safe_case_id, sha256_values};

const EXPECTED_CASES: usize = 124;

/// Scale's Codebase QnA suite with its harness-level agent network restriction made explicit.
#[derive(Clone, Debug)]
pub struct SweAtlasQna {
    root: PathBuf,
    revision: String,
}

impl SweAtlasQna {
    /// Creates an importer for the pinned official `data/qa` directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, revision: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            revision: revision.into(),
        }
    }

    fn tasks(&self) -> Result<Vec<Task>, ImportError> {
        let mut roots = Vec::new();
        for entry in ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .parents(true)
            .follow_links(false)
            .build()
        {
            let entry = entry.map_err(|error| {
                ImportError::Invalid(format!("failed to walk {}: {error}", self.root.display()))
            })?;
            if entry.file_type().is_some_and(|kind| kind.is_file())
                && entry.file_name() == "task.toml"
                && let Some(parent) = entry.path().parent()
            {
                roots.push(parent.to_path_buf());
            }
        }
        roots.sort();
        roots.dedup();
        let tasks = roots
            .into_iter()
            .map(|root| Task::load(root).map_err(ImportError::from))
            .collect::<Result<Vec<_>, _>>()?;
        if tasks.len() != EXPECTED_CASES {
            return Err(ImportError::Invalid(format!(
                "{} contains {} SWE-Atlas QnA tasks, expected {EXPECTED_CASES}",
                self.root.display(),
                tasks.len()
            )));
        }
        let mut names = HashSet::new();
        for task in &tasks {
            self.validate_task(task)?;
            if !names.insert(task.name().to_owned()) {
                return Err(ImportError::Invalid(format!(
                    "{} contains duplicate task {:?}",
                    self.root.display(),
                    task.name()
                )));
            }
        }
        Ok(tasks)
    }

    fn validate_task(&self, task: &Task) -> Result<(), ImportError> {
        if task.network() != NetworkPolicy::Public
            || task.output() != TaskOutput::Workspace
            || task.requires_compose()
            || !task.artifacts().is_empty()
        {
            return Err(ImportError::Invalid(format!(
                "SWE-Atlas QnA task {:?} no longer has the supported official execution shape",
                task.name()
            )));
        }
        let answer = "/logs/agent/answer.txt";
        if !task.prompt().contains(answer) {
            return Err(ImportError::Invalid(format!(
                "SWE-Atlas QnA task {:?} does not declare its answer artifact",
                task.name()
            )));
        }
        Ok(())
    }

    fn verifier_dockerfile(task: &Task) -> Vec<u8> {
        format!("FROM {}\n", task.image().reference()).into_bytes()
    }

    fn case(&self, task: &Task) -> Result<CasePlan, ImportError> {
        let id = safe_case_id(task.name());
        let prompt_chars = u64::try_from(task.prompt().chars().count()).map_err(|_| {
            ImportError::Invalid(format!("SWE-Atlas prompt {:?} is too large", task.name()))
        })?;
        Ok(CasePlan::hermetic(
            id,
            task.prompt(),
            Environment::OciImage(task.image().reference().to_owned()),
            Harness::directory(task.root().join("tests"))?,
        )?
        .benchmark_prompt_chars(prompt_chars)
        .benchmark_case_type("Codebase QnA")
        .output(TaskOutput::FinalMessage)
        .resources(task.resources().clone())
        .timeouts(task.agent_timeout(), task.verifier().timeout())
        .scoring_policy(ScoringPolicy::AllRewardsOne)
        .allow_internet(false)
        .verifier_allow_internet(true)
        .artifact("/logs/agent/answer.txt")?
        .harness_file("Dockerfile", Self::verifier_dockerfile(task), 0o644)?
        .into())
    }
}

impl DatasetImporter for SweAtlasQna {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let tasks = self.tasks()?;
        let source = SourceIdentity::new(
            "swe-atlas-qna",
            &self.revision,
            sha256_values(tasks.iter().map(|task| task.package_digest().as_bytes())),
        )?;
        let mut plan = DatasetPlan::new("swe-atlas-qna", source)?;
        for task in &tasks {
            plan = plan.case(self.case(task)?);
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use nanocodex_eval::{NetworkPolicy, TaskOutput, VerifierEnvironmentMode, import::ImportStore};

    use super::{EXPECTED_CASES, SweAtlasQna};

    #[test]
    fn imports_the_complete_shape_with_agent_network_disabled_and_answer_isolated() {
        let source = tempfile::tempdir().unwrap();
        for index in 0..EXPECTED_CASES {
            make_task(source.path(), index);
        }
        let store = tempfile::tempdir().unwrap();
        let imported = ImportStore::new(store.path())
            .import(&SweAtlasQna::new(source.path(), "swe-atlas@test"))
            .unwrap();

        assert_eq!(imported.tasks().len(), EXPECTED_CASES);
        let task = &imported.tasks()[0];
        assert_eq!(task.network(), NetworkPolicy::Disabled);
        assert_eq!(task.verifier().network(), NetworkPolicy::Public);
        assert_eq!(
            task.verifier().environment_mode(),
            VerifierEnvironmentMode::Separate
        );
        assert_eq!(task.output(), TaskOutput::FinalMessage);
        assert_eq!(task.artifacts().len(), 1);
        assert_eq!(
            task.artifacts()[0].source(),
            Path::new("/logs/agent/answer.txt")
        );
        assert_eq!(
            fs::read_to_string(task.root().join("tests/Dockerfile")).unwrap(),
            "FROM python:3.12-slim\n"
        );
        assert!(!task.root().join("solution").exists());
    }

    fn make_task(root: &Path, index: usize) {
        let task = root.join(format!("task-{index:03}"));
        fs::create_dir_all(task.join("environment")).unwrap();
        fs::create_dir_all(task.join("tests")).unwrap();
        fs::write(
            task.join("task.toml"),
            format!(
                r#"schema_version = "1.1"

[task]
name = "scale-ai/task-{index:03}"

[verifier]
timeout_sec = 30.0

[agent]
timeout_sec = 60.0

[environment]
docker_image = "python:3.12-slim"
cpus = 2
memory_mb = 1024
storage_mb = 2048
gpus = 0
allow_internet = true
"#
            ),
        )
        .unwrap();
        fs::write(
            task.join("instruction.md"),
            "Inspect /app and write the result to /logs/agent/answer.txt.",
        )
        .unwrap();
        fs::write(task.join("environment/Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(
            task.join("tests/test.sh"),
            "#!/bin/sh\nprintf '1\\n' > /logs/verifier/reward.txt\n",
        )
        .unwrap();
    }
}
