use std::{collections::BTreeSet, fs::File, path::PathBuf, time::Duration};

use nanocodex_eval::{
    PromptMessage, Resources, ScoringPolicy, TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};
use parquet::{
    file::reader::{FileReader as _, SerializedFileReader},
    record::{Row, RowAccessor as _},
};
use serde::{Deserialize, Serialize};

use crate::{sha256_file, sha256_values};

const FILES: [(&str, &str); 6] = [
    ("2needle-0", "2needle/2needle_0.parquet"),
    ("2needle-1", "2needle/2needle_1.parquet"),
    ("4needle-0", "4needle/4needle_0.parquet"),
    ("4needle-1", "4needle/4needle_1.parquet"),
    ("8needle-0", "8needle/8needle_0.parquet"),
    ("8needle-1", "8needle/8needle_1.parquet"),
];
const HARNESS_FILES: [&str; 3] = ["Dockerfile", "test.sh", "grade.py"];

/// OpenAI's public multi-round co-reference resolution dataset and grader.
#[derive(Clone, Debug)]
pub struct Mrcr {
    source: PathBuf,
    revision: String,
    environment: Environment,
    harness: PathBuf,
    tasks: Option<BTreeSet<String>>,
}

impl Mrcr {
    /// Creates an importer for one pinned public MRCR snapshot.
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
            tasks: None,
        }
    }

    /// Restricts normalization to the requested stable task IDs.
    #[must_use]
    pub fn tasks(mut self, task_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tasks = Some(task_ids.into_iter().map(Into::into).collect());
        self
    }

    fn includes_partition(&self, partition: &str) -> bool {
        self.tasks.as_ref().is_none_or(|tasks| {
            let prefix = format!("{partition}-");
            tasks.iter().any(|task| task.starts_with(&prefix))
        })
    }

    fn includes_case(&self, id: &str) -> bool {
        self.tasks.as_ref().is_none_or(|tasks| tasks.contains(id))
    }
}

impl DatasetImporter for Mrcr {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let mut source_values = Vec::with_capacity(FILES.len() + HARNESS_FILES.len());
        for relative in HARNESS_FILES {
            source_values.push(sha256_file(&self.harness.join(relative))?);
        }
        let harness = Harness::directory(&self.harness)?;
        let mut cases = Vec::with_capacity(self.tasks.as_ref().map_or(2_400, BTreeSet::len));
        for (partition, relative) in FILES {
            let path = self.source.join(relative);
            source_values.push(sha256_file(&path)?);
            if !self.includes_partition(partition) {
                continue;
            }
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
                let id = format!("{partition}-{index:06}");
                if !self.includes_case(&id) {
                    continue;
                }
                let row = row.map_err(|error| {
                    ImportError::Invalid(format!(
                        "failed to decode {} row {}: {error}",
                        path.display(),
                        index + 1
                    ))
                })?;
                let case = MrcrCase::from_row(&path, index, &row)?;
                let expected =
                    serde_json::to_vec(&case.expected).map_err(|source| ImportError::Json {
                        path: path.clone(),
                        source,
                    })?;
                cases.push(
                    CasePlan::hermetic(
                        id,
                        case.instruction,
                        self.environment.clone(),
                        harness.clone(),
                    )?
                    .transcript(case.transcript)
                    .benchmark_prompt_chars(case.expected.n_chars)
                    .benchmark_case_type(format!("{}-needle", case.expected.n_needles))
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
            "openai-mrcr",
            &self.revision,
            sha256_values(source_values.iter().map(String::as_bytes)),
        )?;
        let mut plan = DatasetPlan::new("mrcr-v2", source)?;
        for case in cases {
            plan = plan.case(case);
        }
        Ok(plan)
    }
}

#[derive(Debug)]
struct MrcrCase {
    transcript: Vec<PromptMessage>,
    instruction: String,
    expected: MrcrExpected,
}

#[derive(Debug, Serialize)]
struct MrcrExpected {
    answer: String,
    prefix: String,
    n_needles: u64,
    desired_message_index: u64,
    total_messages: u64,
    n_chars: u64,
    date_added: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMessage {
    role: RawRole,
    content: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawRole {
    User,
    Assistant,
}

impl MrcrCase {
    fn from_row(path: &std::path::Path, index: usize, row: &Row) -> Result<Self, ImportError> {
        let text = |name: &str| -> Result<String, ImportError> {
            let field = field(path, index, row, name)?;
            row.get_string(field).cloned().map_err(|error| {
                ImportError::Invalid(format!(
                    "{} row {} {name} is not text: {error}",
                    path.display(),
                    index + 1
                ))
            })
        };
        let integer = |name: &str| -> Result<i64, ImportError> {
            let field = field(path, index, row, name)?;
            row.get_long(field).map_err(|error| {
                ImportError::Invalid(format!(
                    "{} row {} {name} is not an integer: {error}",
                    path.display(),
                    index + 1
                ))
            })
        };
        let prompt = text("prompt")?;
        let mut messages = serde_json::from_str::<Vec<RawMessage>>(&prompt).map_err(|source| {
            ImportError::Invalid(format!(
                "failed to decode {} row {} prompt messages: {source}",
                path.display(),
                index + 1
            ))
        })?;
        let final_message = messages.pop().ok_or_else(|| {
            ImportError::Invalid(format!(
                "{} row {} has an empty MRCR transcript",
                path.display(),
                index + 1
            ))
        })?;
        if !matches!(final_message.role, RawRole::User) || final_message.content.trim().is_empty() {
            return Err(ImportError::Invalid(format!(
                "{} row {} must end in a non-empty user instruction",
                path.display(),
                index + 1
            )));
        }
        let total_messages =
            positive_u64(path, index, "total_messages", integer("total_messages")?)?;
        if usize::try_from(total_messages).ok() != Some(messages.len() + 1) {
            return Err(ImportError::Invalid(format!(
                "{} row {} declares {total_messages} messages but contains {}",
                path.display(),
                index + 1,
                messages.len() + 1
            )));
        }
        let transcript = messages
            .into_iter()
            .map(|message| match message.role {
                RawRole::User => PromptMessage::user(message.content),
                RawRole::Assistant => PromptMessage::assistant(message.content),
            })
            .collect::<Vec<_>>();
        let n_needles = positive_u64(path, index, "n_needles", integer("n_needles")?)?;
        if !matches!(n_needles, 2 | 4 | 8) {
            return Err(ImportError::Invalid(format!(
                "{} row {} declares unsupported n_needles {n_needles}",
                path.display(),
                index + 1
            )));
        }
        let prefix = text("random_string_to_prepend")?;
        if prefix.chars().count() != 10 || !prefix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(ImportError::Invalid(format!(
                "{} row {} has an invalid ten-character alphanumeric answer prefix",
                path.display(),
                index + 1
            )));
        }
        let answer = text("answer")?;
        if !answer.starts_with(&prefix) {
            return Err(ImportError::Invalid(format!(
                "{} row {} answer does not begin with its required prefix",
                path.display(),
                index + 1
            )));
        }
        Ok(Self {
            transcript,
            instruction: final_message.content,
            expected: MrcrExpected {
                answer,
                prefix,
                n_needles,
                desired_message_index: positive_u64(
                    path,
                    index,
                    "desired_msg_index",
                    integer("desired_msg_index")?,
                )?,
                total_messages,
                n_chars: positive_u64(path, index, "n_chars", integer("n_chars")?)?,
                date_added: text("date_added")?,
            },
        })
    }
}

fn field(
    path: &std::path::Path,
    index: usize,
    row: &Row,
    name: &str,
) -> Result<usize, ImportError> {
    row.get_column_iter()
        .position(|(candidate, _)| candidate == name)
        .ok_or_else(|| {
            ImportError::Invalid(format!(
                "{} row {} has no {name:?} column",
                path.display(),
                index + 1
            ))
        })
}

fn positive_u64(
    path: &std::path::Path,
    index: usize,
    name: &str,
    value: i64,
) -> Result<u64, ImportError> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            ImportError::Invalid(format!(
                "{} row {} declares invalid {name} {value}",
                path.display(),
                index + 1
            ))
        })
}

#[cfg(test)]
mod tests {
    use nanocodex_eval::{PromptMessageRole, import::Environment};
    use parquet::record::{Field, Row};

    use super::{Mrcr, MrcrCase};

    #[test]
    fn task_selection_skips_unrequested_partitions_and_rows() {
        let importer = Mrcr::new(
            "source",
            "revision",
            Environment::OciImage("python:3.12-slim".to_owned()),
            "harness",
        )
        .tasks(["8needle-0-000000"]);

        assert!(!importer.includes_partition("2needle-0"));
        assert!(importer.includes_partition("8needle-0"));
        assert!(importer.includes_case("8needle-0-000000"));
        assert!(!importer.includes_case("8needle-0-000001"));
    }

    #[test]
    fn preserves_alternating_messages_and_official_dimensions() {
        let prompt = serde_json::json!([
            {"role": "user", "content": "write a poem"},
            {"role": "assistant", "content": "first poem"},
            {"role": "user", "content": "write a poem"},
            {"role": "assistant", "content": "second poem"},
            {"role": "user", "content": "Prepend abc1234567 to the second poem."}
        ])
        .to_string();
        let row = Row::new(vec![
            ("prompt".to_owned(), Field::Str(prompt)),
            (
                "answer".to_owned(),
                Field::Str("abc1234567second poem".to_owned()),
            ),
            (
                "random_string_to_prepend".to_owned(),
                Field::Str("abc1234567".to_owned()),
            ),
            ("n_needles".to_owned(), Field::Long(2)),
            ("desired_msg_index".to_owned(), Field::Long(3)),
            ("total_messages".to_owned(), Field::Long(5)),
            ("n_chars".to_owned(), Field::Long(1234)),
            ("date_added".to_owned(), Field::Str("2025-12-04".to_owned())),
        ]);

        let case = MrcrCase::from_row(std::path::Path::new("fixture.parquet"), 0, &row).unwrap();

        assert_eq!(case.transcript.len(), 4);
        assert_eq!(case.transcript[0].role(), PromptMessageRole::User);
        assert_eq!(case.transcript[1].role(), PromptMessageRole::Assistant);
        assert_eq!(case.instruction, "Prepend abc1234567 to the second poem.");
        assert_eq!(case.expected.n_needles, 2);
        assert_eq!(case.expected.n_chars, 1234);
        assert_eq!(case.expected.total_messages, 5);
    }

    #[test]
    fn rejects_a_declared_message_count_that_changes_benchmark_shape() {
        let row = Row::new(vec![
            (
                "prompt".to_owned(),
                Field::Str(
                    serde_json::json!([
                        {"role": "user", "content": "question"},
                        {"role": "assistant", "content": "answer"},
                        {"role": "user", "content": "repeat it"}
                    ])
                    .to_string(),
                ),
            ),
            ("answer".to_owned(), Field::Str("prefixanswer".to_owned())),
            (
                "random_string_to_prepend".to_owned(),
                Field::Str("prefix".to_owned()),
            ),
            ("n_needles".to_owned(), Field::Long(2)),
            ("desired_msg_index".to_owned(), Field::Long(1)),
            ("total_messages".to_owned(), Field::Long(4)),
            ("n_chars".to_owned(), Field::Long(100)),
            ("date_added".to_owned(), Field::Str("2025-12-04".to_owned())),
        ]);

        let error =
            MrcrCase::from_row(std::path::Path::new("fixture.parquet"), 0, &row).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("declares 4 messages but contains 3")
        );
    }
}
