use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use nanocodex_eval::{
    TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};

use crate::{read_json_lines, safe_case_id, sha256_file};

const FINAL_RESPONSE_INSTRUCTIONS: &str = "Return the complete answer in the final assistant \
message. The benchmark judge sees only that message and cannot inspect files in the workspace, \
so do not refer to local artifacts as the answer.";

/// Arena-Hard prompt importer using a caller-packaged official judge harness.
#[derive(Clone, Debug)]
pub struct ArenaHard {
    name: Box<str>,
    questions: PathBuf,
    revision: Box<str>,
    environment: Environment,
    harness: Harness,
    baseline_answers: Option<PathBuf>,
}

impl ArenaHard {
    /// Creates an Arena-Hard importer.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        questions_jsonl: impl Into<PathBuf>,
        revision: impl Into<String>,
        environment: Environment,
        harness: Harness,
    ) -> Self {
        Self {
            name: name.into().into_boxed_str(),
            questions: questions_jsonl.into(),
            revision: revision.into().into_boxed_str(),
            environment,
            harness,
            baseline_answers: None,
        }
    }

    /// Binds the benchmark's published baseline answers into every judged case.
    #[must_use]
    pub fn baseline_answers(mut self, path: impl Into<PathBuf>) -> Self {
        self.baseline_answers = Some(path.into());
        self
    }
}

impl DatasetImporter for ArenaHard {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let questions = read_json_lines::<ArenaQuestion>(&self.questions)?;
        let baselines = self
            .baseline_answers
            .as_ref()
            .map(|path| read_json_lines::<ArenaAnswer>(path))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|answer| (answer.uid.clone(), answer))
            .collect::<BTreeMap<_, _>>();
        let source_digest = if let Some(path) = &self.baseline_answers {
            crate::sha256_values([sha256_file(&self.questions)?, sha256_file(path)?])
        } else {
            sha256_file(&self.questions)?
        };
        let source = SourceIdentity::new("arena-hard", self.revision.as_ref(), source_digest)?;
        let mut plan = DatasetPlan::new(self.name.as_ref(), source)?;
        for question in questions {
            let metadata = serde_json::to_vec(&question).map_err(|source| ImportError::Json {
                path: self.questions.clone(),
                source,
            })?;
            let uid = question.uid.clone();
            let mut case = CasePlan::hermetic(
                safe_case_id(&question.uid),
                question.prompt.clone(),
                self.environment.clone(),
                self.harness.clone(),
            )?
            .instructions(FINAL_RESPONSE_INSTRUCTIONS)
            .output(TaskOutput::FinalMessage)
            .harness_file("case.json", metadata, 0o600)?;
            if let Some(baseline) = baselines.get(&uid) {
                let baseline =
                    serde_json::to_vec(baseline).map_err(|source| ImportError::Json {
                        path: self
                            .baseline_answers
                            .clone()
                            .unwrap_or_else(|| self.questions.clone()),
                        source,
                    })?;
                case = case.harness_file("baseline.json", baseline, 0o600)?;
            } else if self.baseline_answers.is_some() {
                return Err(ImportError::Invalid(format!(
                    "Arena-Hard baseline answer is missing for {uid}"
                )));
            }
            plan = plan.case(case);
        }
        Ok(plan)
    }
}

#[derive(Deserialize, Serialize)]
struct ArenaQuestion {
    uid: String,
    category: String,
    #[serde(default)]
    subcategory: Option<String>,
    prompt: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct ArenaAnswer {
    uid: String,
    model: String,
    messages: Vec<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use nanocodex_eval::import::ImportStore;

    use super::*;

    #[test]
    fn imports_final_message_case_and_metadata() {
        let source = tempfile::tempdir().unwrap();
        let questions = source.path().join("questions.jsonl");
        fs::write(
            &questions,
            "{\"uid\":\"q-1\",\"category\":\"hard\",\"prompt\":\"Explain it.\"}\n",
        )
        .unwrap();
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/arena-hard");
        let store = tempfile::tempdir().unwrap();

        let imported = ImportStore::new(store.path())
            .import(&ArenaHard::new(
                "arena-hard-v2",
                questions,
                "arena@fixture",
                Environment::OciImage("debian:bookworm-slim".to_owned()),
                Harness::directory(assets).unwrap(),
            ))
            .unwrap();

        assert_eq!(imported.tasks().len(), 1);
        assert_eq!(imported.tasks()[0].output(), TaskOutput::FinalMessage);
        assert!(imported.tasks()[0].root().join("tests/case.json").is_file());
    }
}
