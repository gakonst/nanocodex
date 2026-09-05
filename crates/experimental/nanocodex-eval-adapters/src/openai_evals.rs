use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_yaml_ng::{Mapping, Value};

use nanocodex_eval::{
    TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};

use crate::{read_json_lines, safe_case_id, sha256_file, sha256_values};

const MATCH_CLASS: &str = "evals.elsuite.basic.match:Match";
const INCLUDES_CLASS: &str = "evals.elsuite.basic.includes:Includes";
const HARNESS_FILES: &[&str] = &["Dockerfile", "test.sh", "grade.py"];

/// Importer for the declarative `Match` and `Includes` families in OpenAI
/// Evals. Custom Python eval classes must use [`crate::ExternalHarness`], which
/// preserves their official implementation instead of guessing semantics.
#[derive(Clone, Debug)]
pub struct OpenAiEvals {
    name: Box<str>,
    registry: PathBuf,
    harness: PathBuf,
    eval: Box<str>,
    revision: Box<str>,
    environment: Environment,
}

impl OpenAiEvals {
    /// Creates an importer for one registry eval ID.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        registry: impl Into<PathBuf>,
        harness: impl Into<PathBuf>,
        eval: impl Into<String>,
        revision: impl Into<String>,
        environment: Environment,
    ) -> Self {
        Self {
            name: name.into().into_boxed_str(),
            registry: registry.into(),
            harness: harness.into(),
            eval: eval.into().into_boxed_str(),
            revision: revision.into().into_boxed_str(),
            environment,
        }
    }
}

impl DatasetImporter for OpenAiEvals {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        validate_harness(&self.harness)?;
        let definition = find_definition(&self.registry, &self.eval)?;
        let class = string_field(&definition.value, "class")?;
        let args = mapping_field(&definition.value, "args")?;
        if args.contains_key(Value::String("num_few_shot".to_owned())) {
            return Err(ImportError::Invalid(
                "OpenAI Evals few-shot prompt rewriting requires the official external harness"
                    .to_owned(),
            ));
        }
        let samples = string_from_mapping(args, "samples_jsonl")?;
        let sample_path = self.registry.join("data").join(samples);
        let mode = match class.as_str() {
            MATCH_CLASS => "match",
            INCLUDES_CLASS => {
                if bool_from_mapping(args, "ignore_case")?.unwrap_or(false) {
                    "includes_ignore_case"
                } else {
                    "includes"
                }
            }
            _ => {
                return Err(ImportError::Invalid(format!(
                    "OpenAI Evals class {class:?} owns executable semantics; import it through an external harness"
                )));
            }
        };
        let rows = read_json_lines::<EvalSample>(&sample_path)?;
        let mut source_inputs = vec![
            self.eval.to_string(),
            sha256_file(&definition.path)?,
            sha256_file(&sample_path)?,
        ];
        for relative in HARNESS_FILES {
            source_inputs.push(sha256_file(&self.harness.join(relative))?);
        }
        let source_digest = sha256_values(source_inputs.iter().map(String::as_bytes));
        let source = SourceIdentity::new("openai-evals", self.revision.as_ref(), source_digest)?;
        let harness = Harness::directory(&self.harness)?;
        let mut plan = DatasetPlan::new(self.name.as_ref(), source)?;
        for (index, sample) in rows.into_iter().enumerate() {
            let prompt = sample.prompt()?;
            let expected = sample.ideal_strings()?;
            let expected = serde_json::to_vec(&expected).map_err(|source| ImportError::Json {
                path: sample_path.clone(),
                source,
            })?;
            let id = format!("{}-{index:06}", safe_case_id(&self.eval));
            plan = plan.case({
                let mut case =
                    CasePlan::hermetic(id, prompt.user, self.environment.clone(), harness.clone())?;
                if let Some(instructions) = prompt.instructions {
                    case = case.instructions(instructions);
                }
                case.output(TaskOutput::FinalMessage)
                    .harness_file("expected.json", expected, 0o600)?
                    .harness_file("mode", mode.as_bytes().to_vec(), 0o600)?
            });
        }
        Ok(plan)
    }
}

fn validate_harness(harness: &std::path::Path) -> Result<(), ImportError> {
    for relative in HARNESS_FILES {
        let path = harness.join(relative);
        if !path.is_file() {
            return Err(ImportError::Invalid(format!(
                "OpenAI Evals harness is missing {}",
                path.display()
            )));
        }
    }
    Ok(())
}

struct EvalDefinition {
    path: PathBuf,
    value: Mapping,
}

fn find_definition(registry: &Path, eval: &str) -> Result<EvalDefinition, ImportError> {
    let root = registry.join("evals");
    for entry in fs::read_dir(&root).map_err(|source| ImportError::Io {
        path: root.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| ImportError::Io {
            path: root.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("yaml") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|source| ImportError::Io {
            path: path.clone(),
            source,
        })?;
        let document: Mapping = serde_yaml_ng::from_str(&text).map_err(|source| {
            ImportError::Invalid(format!("failed to decode {}: {source}", path.display()))
        })?;
        if let Some(Value::Mapping(value)) = document.get(Value::String(eval.to_owned())) {
            return Ok(EvalDefinition {
                path,
                value: value.clone(),
            });
        }
    }
    Err(ImportError::Invalid(format!(
        "OpenAI Evals registry does not define {eval:?}"
    )))
}

fn string_field(mapping: &Mapping, name: &str) -> Result<String, ImportError> {
    string_from_mapping(mapping, name)
}

fn mapping_field<'a>(mapping: &'a Mapping, name: &str) -> Result<&'a Mapping, ImportError> {
    mapping
        .get(Value::String(name.to_owned()))
        .and_then(Value::as_mapping)
        .ok_or_else(|| ImportError::Invalid(format!("OpenAI Evals field {name:?} must be a map")))
}

fn string_from_mapping(mapping: &Mapping, name: &str) -> Result<String, ImportError> {
    mapping
        .get(Value::String(name.to_owned()))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ImportError::Invalid(format!("OpenAI Evals field {name:?} must be a string"))
        })
}

fn bool_from_mapping(mapping: &Mapping, name: &str) -> Result<Option<bool>, ImportError> {
    match mapping.get(Value::String(name.to_owned())) {
        None => Ok(None),
        Some(value) => value.as_bool().map(Some).ok_or_else(|| {
            ImportError::Invalid(format!("OpenAI Evals field {name:?} must be a boolean"))
        }),
    }
}

#[derive(Deserialize)]
struct EvalSample {
    input: serde_json::Value,
    ideal: serde_json::Value,
}

impl EvalSample {
    fn prompt(&self) -> Result<EvalPrompt, ImportError> {
        if let Some(prompt) = self.input.as_str() {
            return Ok(EvalPrompt {
                instructions: None,
                user: prompt.to_owned(),
            });
        }
        let messages = self.input.as_array().ok_or_else(|| {
            ImportError::Invalid(
                "OpenAI Evals input must be text or one system message followed by one user message"
                    .to_owned(),
            )
        })?;
        if messages.is_empty() || messages.len() > 2 {
            return Err(ImportError::Invalid(
                "OpenAI Evals conversations beyond one system and one user message require an official external harness"
                    .to_owned(),
            ));
        }
        let user = messages
            .last()
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ImportError::Invalid("OpenAI Evals chat input must contain objects".to_owned())
            })?;
        if user.get("role").and_then(serde_json::Value::as_str) != Some("user") {
            return Err(ImportError::Invalid(
                "OpenAI Evals final message must be user".to_owned(),
            ));
        }
        let user = user
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ImportError::Invalid("chat content must be text".to_owned()))?;
        let instructions = if messages.len() == 2 {
            let system = messages[0].as_object().ok_or_else(|| {
                ImportError::Invalid("OpenAI Evals chat input must contain objects".to_owned())
            })?;
            if system.get("role").and_then(serde_json::Value::as_str) != Some("system") {
                return Err(ImportError::Invalid(
                    "OpenAI Evals first message must be system when two messages are present"
                        .to_owned(),
                ));
            }
            Some(
                system
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| ImportError::Invalid("chat content must be text".to_owned()))?
                    .to_owned(),
            )
        } else {
            None
        };
        Ok(EvalPrompt { instructions, user })
    }

    fn ideal_strings(&self) -> Result<Vec<String>, ImportError> {
        if let Some(ideal) = self.ideal.as_str() {
            return Ok(vec![ideal.to_owned()]);
        }
        self.ideal
            .as_array()
            .filter(|values| values.iter().all(serde_json::Value::is_string))
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .ok_or_else(|| {
                ImportError::Invalid("OpenAI Evals ideal must be text or text array".to_owned())
            })
    }
}

struct EvalPrompt {
    instructions: Option<String>,
    user: String,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use nanocodex_eval::import::ImportStore;

    use super::*;

    #[test]
    fn imports_match_without_reimplementing_custom_classes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("evals")).unwrap();
        fs::create_dir_all(root.path().join("data/demo")).unwrap();
        fs::write(
            root.path().join("evals/demo.yaml"),
            r#"demo.match-v1:
  class: evals.elsuite.basic.match:Match
  args:
    samples_jsonl: demo/samples.jsonl
"#,
        )
        .unwrap();
        fs::write(
            root.path().join("data/demo/samples.jsonl"),
            "{\"input\":[{\"role\":\"user\",\"content\":\"2 + 2?\"}],\"ideal\":[\"4\",\"four\"]}\n",
        )
        .unwrap();
        let store = tempfile::tempdir().unwrap();
        let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/openai-evals");

        let imported = ImportStore::new(store.path())
            .import(&OpenAiEvals::new(
                "openai-demo",
                root.path(),
                harness,
                "demo.match-v1",
                "openai-evals@fixture",
                Environment::OciImage("python:3.12-slim".to_owned()),
            ))
            .unwrap();

        assert_eq!(imported.tasks()[0].prompt(), "2 + 2?");
        assert_eq!(imported.tasks()[0].output(), TaskOutput::FinalMessage);
        assert!(
            imported.tasks()[0]
                .root()
                .join("tests/expected.json")
                .is_file()
        );
    }
}
