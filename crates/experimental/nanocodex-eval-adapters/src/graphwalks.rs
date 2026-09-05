use std::{fs::File, path::PathBuf, time::Duration};

use nanocodex_eval::{
    Resources, ScoringPolicy, TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};
use parquet::{
    file::reader::{FileReader as _, SerializedFileReader},
    record::{ListAccessor as _, Row, RowAccessor as _},
};

use crate::{sha256_file, sha256_values};

const FILES: [(&str, &str); 2] = [
    ("128k-and-shorter", "graphwalks_128k_and_shorter.parquet"),
    ("256k-to-1mil", "graphwalks_256k_to_1mil.parquet"),
];
const HARNESS_FILES: [&str; 3] = ["Dockerfile", "test.sh", "grade.py"];

/// OpenAI's public GraphWalks long-context dataset and published F1 grader.
#[derive(Clone, Debug)]
pub struct GraphWalks {
    source: PathBuf,
    revision: String,
    environment: Environment,
    harness: PathBuf,
}

impl GraphWalks {
    /// Creates an importer for one pinned public dataset snapshot.
    #[must_use]
    pub fn new(
        source: impl Into<PathBuf>,
        revision: impl Into<String>,
        environment: Environment,
        harness: impl Into<PathBuf>,
    ) -> Self {
        Self {
            source: source.into(),
            revision: revision.into(),
            environment,
            harness: harness.into(),
        }
    }
}

impl DatasetImporter for GraphWalks {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let mut source_values = Vec::with_capacity(FILES.len() + HARNESS_FILES.len());
        for relative in HARNESS_FILES {
            source_values.push(sha256_file(&self.harness.join(relative))?);
        }
        let harness = Harness::directory(&self.harness)?;
        let mut cases = Vec::with_capacity(1_150);
        for (partition, relative) in FILES {
            let path = self.source.join(relative);
            source_values.push(sha256_file(&path)?);
            let file = File::open(&path).map_err(|source| ImportError::Io {
                path: path.clone(),
                source,
            })?;
            let reader = SerializedFileReader::new(file).map_err(|error| {
                ImportError::Invalid(format!("failed to open {}: {error}", path.display()))
            })?;
            let rows = reader.get_row_iter(None).map_err(|error| {
                ImportError::Invalid(format!("failed to read {}: {error}", path.display()))
            })?;
            for (index, row) in rows.enumerate() {
                let row = row.map_err(|error| {
                    ImportError::Invalid(format!(
                        "failed to decode {} row {}: {error}",
                        path.display(),
                        index + 1
                    ))
                })?;
                let case = GraphWalksCase::from_row(&path, index, &row)?;
                let expected =
                    serde_json::to_vec(&case.answer).map_err(|source| ImportError::Json {
                        path: path.clone(),
                        source,
                    })?;
                cases.push(
                    CasePlan::hermetic(
                        format!("{partition}-{index:06}"),
                        case.prompt,
                        self.environment.clone(),
                        harness.clone(),
                    )?
                    .benchmark_prompt_chars(case.prompt_chars)
                    .benchmark_case_type(case.problem_type)
                    .output(TaskOutput::FinalMessage)
                    .resources(Resources {
                        cpus: 2,
                        memory_mb: 4_096,
                        storage_mb: 4_096,
                        gpus: 0,
                    })
                    .timeouts(Duration::from_secs(1_800), Duration::from_secs(60))
                    .scoring_policy(ScoringPolicy::AllRewardsOne)
                    .harness_file("expected.json", expected, 0o600)?,
                );
            }
        }
        let source = SourceIdentity::new(
            "openai-graphwalks",
            &self.revision,
            sha256_values(source_values.iter().map(String::as_bytes)),
        )?;
        let mut plan = DatasetPlan::new("graphwalks", source)?;
        for case in cases {
            plan = plan.case(case);
        }
        Ok(plan)
    }
}

struct GraphWalksCase {
    prompt: String,
    prompt_chars: u64,
    problem_type: String,
    answer: Vec<String>,
}

impl GraphWalksCase {
    fn from_row(path: &std::path::Path, index: usize, row: &Row) -> Result<Self, ImportError> {
        let field = |name: &str| {
            row.get_column_iter()
                .position(|(candidate, _)| candidate == name)
                .ok_or_else(|| {
                    ImportError::Invalid(format!(
                        "{} row {} has no {name:?} column",
                        path.display(),
                        index + 1
                    ))
                })
        };
        let prompt_index = field("prompt")?;
        let answer_index = field("answer_nodes").or_else(|_| field("answer"))?;
        let prompt_chars_index = field("prompt_chars")?;
        let problem_type_index = field("problem_type")?;
        let prompt = row.get_string(prompt_index).map_err(|error| {
            ImportError::Invalid(format!(
                "{} row {} prompt is not text: {error}",
                path.display(),
                index + 1
            ))
        })?;
        let prompt_chars = row.get_long(prompt_chars_index).map_err(|error| {
            ImportError::Invalid(format!(
                "{} row {} prompt_chars is not an integer: {error}",
                path.display(),
                index + 1
            ))
        })?;
        let prompt_chars = u64::try_from(prompt_chars).map_err(|_| {
            ImportError::Invalid(format!(
                "{} row {} declares invalid prompt_chars {prompt_chars}",
                path.display(),
                index + 1
            ))
        })?;
        if prompt_chars == 0 {
            return Err(ImportError::Invalid(format!(
                "{} row {} declares zero prompt_chars",
                path.display(),
                index + 1
            )));
        }
        let problem_type = row.get_string(problem_type_index).map_err(|error| {
            ImportError::Invalid(format!(
                "{} row {} problem_type is not text: {error}",
                path.display(),
                index + 1
            ))
        })?;
        if problem_type.trim().is_empty() {
            return Err(ImportError::Invalid(format!(
                "{} row {} has an empty GraphWalks problem type",
                path.display(),
                index + 1
            )));
        }
        let answers = row.get_list(answer_index).map_err(|error| {
            ImportError::Invalid(format!(
                "{} row {} answer is not a string list: {error}",
                path.display(),
                index + 1
            ))
        })?;
        let answer = (0..answers.len())
            .map(|answer| {
                answers.get_string(answer).cloned().map_err(|error| {
                    ImportError::Invalid(format!(
                        "{} row {} answer contains non-text: {error}",
                        path.display(),
                        index + 1
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            prompt: prompt.clone(),
            prompt_chars,
            problem_type: problem_type.clone(),
            answer,
        })
    }
}
