use std::{collections::HashSet, path::PathBuf, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex_eval::{
    Resources, ScoringPolicy, TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{sha256_file, sha256_values};

const EXPECTED_CASES: usize = 1_266;
const HARNESS_FILES: [&str; 3] = ["Dockerfile", "test.sh", "grade.py"];
const QUERY_SUFFIX: &str = "Your response should be in the following format:\nExplanation: {your explanation for your final answer}\nExact Answer: {your succinct, final answer}\nConfidence: {your confidence score between 0% and 100% for your answer}";

/// OpenAI's encrypted BrowseComp release and subscription-judge reproduction.
#[derive(Clone, Debug)]
pub struct BrowseComp {
    source: PathBuf,
    revision: String,
    environment: Environment,
    harness: PathBuf,
}

impl BrowseComp {
    /// Creates an importer for the exact official encrypted CSV snapshot.
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

    fn cases(&self) -> Result<Vec<BrowseCompCase>, ImportError> {
        let mut reader = csv::Reader::from_path(&self.source).map_err(|error| {
            ImportError::Invalid(format!("failed to read {}: {error}", self.source.display()))
        })?;
        let headers = reader.headers().map_err(|error| {
            ImportError::Invalid(format!(
                "failed to read {} headers: {error}",
                self.source.display()
            ))
        })?;
        if headers.iter().collect::<Vec<_>>() != ["problem", "answer", "problem_topic", "canary"] {
            return Err(ImportError::Invalid(format!(
                "{} does not have the exact BrowseComp columns",
                self.source.display()
            )));
        }
        let mut cases = Vec::new();
        let mut encrypted_problems = HashSet::new();
        for (index, row) in reader.deserialize::<BrowseCompRow>().enumerate() {
            let row = row.map_err(|error| {
                ImportError::Invalid(format!(
                    "{} row {} is not a BrowseComp record: {error}",
                    self.source.display(),
                    index + 2
                ))
            })?;
            if !encrypted_problems.insert(row.problem.clone()) {
                return Err(ImportError::Invalid(format!(
                    "{} contains a duplicate encrypted problem at row {}",
                    self.source.display(),
                    index + 2
                )));
            }
            cases.push(BrowseCompCase::new(index, row)?);
        }
        if cases.len() != EXPECTED_CASES {
            return Err(ImportError::Invalid(format!(
                "{} contains {} BrowseComp rows, expected {EXPECTED_CASES}",
                self.source.display(),
                cases.len()
            )));
        }
        Ok(cases)
    }
}

impl DatasetImporter for BrowseComp {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let source_values = std::iter::once(sha256_file(&self.source)?)
            .chain(
                HARNESS_FILES
                    .into_iter()
                    .map(|file| sha256_file(&self.harness.join(file)))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .collect::<Vec<_>>();
        let source = SourceIdentity::new(
            "openai-browsecomp",
            &self.revision,
            sha256_values(source_values.iter().map(String::as_bytes)),
        )?;
        let harness = Harness::directory(&self.harness)?;
        let mut plan = DatasetPlan::new("browsecomp", source)?;
        for case in self.cases()? {
            let prompt_chars = u64::try_from(case.prompt.chars().count()).map_err(|_| {
                ImportError::Invalid(format!("BrowseComp prompt {:?} is too large", case.id))
            })?;
            let verifier = serde_json::to_vec(&BrowseCompVerifierCase {
                id: &case.id,
                question: &case.question,
                correct_answer: &case.correct_answer,
                topic: &case.topic,
                scoring: BrowseCompScoring {
                    grader_model: "gpt-5.6-sol",
                    grader_contract: "openai-subscription-judge-reproduction",
                    reference_model: "gpt-4.1-2025-04-14",
                    reference_temperature: 0.5,
                    reference_max_output_tokens: 2_048,
                },
            })
            .map_err(|error| {
                ImportError::Invalid(format!("failed to encode BrowseComp case: {error}"))
            })?;
            let task = CasePlan::hermetic(
                &case.id,
                case.prompt,
                self.environment.clone(),
                harness.clone(),
            )?
            .benchmark_prompt_chars(prompt_chars)
            .benchmark_case_type(case.topic)
            .output(TaskOutput::FinalMessage)
            .scoring_policy(ScoringPolicy::AllRewardsOne)
            .resources(Resources {
                cpus: 1,
                memory_mb: 1_024,
                storage_mb: 1_024,
                gpus: 0,
            })
            .timeouts(Duration::from_secs(3_600), Duration::from_secs(900))
            .allow_internet(true)
            .verifier_allow_internet(true)
            .harness_file("case.json", verifier, 0o600)?;
            plan = plan.case(task);
        }
        Ok(plan)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowseCompRow {
    problem: String,
    answer: String,
    problem_topic: String,
    canary: String,
}

struct BrowseCompCase {
    id: String,
    prompt: String,
    question: String,
    correct_answer: String,
    topic: String,
}

impl BrowseCompCase {
    fn new(index: usize, row: BrowseCompRow) -> Result<Self, ImportError> {
        if row.problem.is_empty()
            || row.answer.is_empty()
            || row.problem_topic.trim().is_empty()
            || row.canary.is_empty()
        {
            return Err(ImportError::Invalid(format!(
                "BrowseComp row {} has an empty field",
                index + 2
            )));
        }
        let question = BrowseCompCiphertext::new(&row.problem, &row.canary).decrypt(index)?;
        let correct_answer = BrowseCompCiphertext::new(&row.answer, &row.canary).decrypt(index)?;
        if question.trim().is_empty() || correct_answer.trim().is_empty() {
            return Err(ImportError::Invalid(format!(
                "BrowseComp row {} decrypts to an empty field",
                index + 2
            )));
        }
        let digest = hex::encode(Sha256::digest(row.problem.as_bytes()));
        let id = format!("{index:06}-{}", &digest[..16]);
        let prompt = format!("{question}\n\n{QUERY_SUFFIX}");
        Ok(Self {
            id,
            prompt,
            question,
            correct_answer,
            topic: row.problem_topic,
        })
    }
}

struct BrowseCompCiphertext<'a> {
    ciphertext: &'a str,
    password: &'a str,
}

impl<'a> BrowseCompCiphertext<'a> {
    const fn new(ciphertext: &'a str, password: &'a str) -> Self {
        Self {
            ciphertext,
            password,
        }
    }

    fn decrypt(&self, index: usize) -> Result<String, ImportError> {
        let encrypted = STANDARD.decode(self.ciphertext).map_err(|error| {
            ImportError::Invalid(format!(
                "BrowseComp row {} has invalid base64 ciphertext: {error}",
                index + 2
            ))
        })?;
        let key = Sha256::digest(self.password.as_bytes());
        let decrypted = encrypted
            .iter()
            .enumerate()
            .map(|(offset, byte)| byte ^ key[offset % key.len()])
            .collect::<Vec<_>>();
        String::from_utf8(decrypted).map_err(|error| {
            ImportError::Invalid(format!(
                "BrowseComp row {} ciphertext is not valid UTF-8: {error}",
                index + 2
            ))
        })
    }
}

#[derive(Serialize)]
struct BrowseCompVerifierCase<'a> {
    id: &'a str,
    question: &'a str,
    correct_answer: &'a str,
    topic: &'a str,
    scoring: BrowseCompScoring,
}

#[derive(Serialize)]
struct BrowseCompScoring {
    grader_model: &'static str,
    grader_contract: &'static str,
    reference_model: &'static str,
    reference_temperature: f64,
    reference_max_output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use nanocodex_eval::import::Environment;
    use sha2::{Digest as _, Sha256};
    use tempfile::tempdir;

    use super::{BrowseComp, BrowseCompCase, BrowseCompRow, EXPECTED_CASES};

    fn encrypt(value: &str, password: &str) -> String {
        let key = Sha256::digest(password.as_bytes());
        let encrypted = value
            .as_bytes()
            .iter()
            .enumerate()
            .map(|(offset, byte)| byte ^ key[offset % key.len()])
            .collect::<Vec<_>>();
        STANDARD.encode(encrypted)
    }

    #[test]
    fn case_decrypts_the_official_xor_shape_without_leaking_the_answer() {
        let password = "private canary";
        let case = BrowseCompCase::new(
            788,
            BrowseCompRow {
                problem: encrypt("Which record is correct?", password),
                answer: encrypt("The hidden answer", password),
                problem_topic: "Records".to_owned(),
                canary: password.to_owned(),
            },
        )
        .unwrap();

        assert_eq!(case.question, "Which record is correct?");
        assert_eq!(case.correct_answer, "The hidden answer");
        assert_eq!(case.topic, "Records");
        assert_eq!(
            case.prompt,
            "Which record is correct?\n\nYour response should be in the following format:\nExplanation: {your explanation for your final answer}\nExact Answer: {your succinct, final answer}\nConfidence: {your confidence score between 0% and 100% for your answer}"
        );
        assert!(!case.prompt.contains("The hidden answer"));
        assert!(!case.prompt.contains(password));
        assert!(case.id.starts_with("000788-"));
    }

    #[test]
    fn malformed_ciphertext_fails_closed() {
        let error = BrowseCompCase::new(
            0,
            BrowseCompRow {
                problem: "%%%".to_owned(),
                answer: "%%%".to_owned(),
                problem_topic: "Records".to_owned(),
                canary: "canary".to_owned(),
            },
        )
        .err()
        .unwrap();

        assert!(error.to_string().contains("invalid base64 ciphertext"));
    }

    #[test]
    fn complete_release_shape_requires_unique_ordered_records() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("browsecomp.csv");
        let mut writer = csv::Writer::from_path(&source).unwrap();
        writer
            .write_record(["problem", "answer", "problem_topic", "canary"])
            .unwrap();
        for index in 0..EXPECTED_CASES {
            let password = format!("canary-{index}");
            writer
                .write_record([
                    encrypt(&format!("Question {index}?"), &password),
                    encrypt(&format!("Answer {index}"), &password),
                    "Synthetic".to_owned(),
                    password,
                ])
                .unwrap();
        }
        writer.flush().unwrap();
        let importer = BrowseComp::new(
            &source,
            "browsecomp@test",
            Environment::OciImage("python:3.12-slim".to_owned()),
            directory.path().join("harness"),
        );

        let cases = importer.cases().unwrap();

        assert_eq!(cases.len(), EXPECTED_CASES);
        assert!(cases[788].id.starts_with("000788-"));
        assert_eq!(cases[788].question, "Question 788?");
    }
}
