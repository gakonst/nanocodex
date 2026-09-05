use std::{
    collections::{BTreeSet, HashSet},
    fs::File,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use nanocodex_eval::{
    Resources, TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};
use parquet::{
    file::reader::{FileReader as _, SerializedFileReader},
    record::{ListAccessor as _, Row, RowAccessor as _},
};
use serde::{Deserialize, Serialize};

use crate::{sha256_file, sha256_values};

pub(crate) const PARQUET_PATH: &str = "data/train-00000-of-00001.parquet";
const HARNESS_FILES: [&str; 3] = ["Dockerfile", "test.sh", "grade.py"];
const EXPECTED_CASES: usize = 220;
const EXPECTED_SECTORS: usize = 9;
const EXPECTED_OCCUPATIONS: usize = 44;
const EXPECTED_RUBRICS: usize = 10_453;
const EXPECTED_REFERENCE_FILES: usize = 261;
const EXPECTED_DELIVERABLE_FILES: usize = 248;
const COLUMNS: [&str; 12] = [
    "task_id",
    "sector",
    "occupation",
    "prompt",
    "reference_files",
    "reference_file_urls",
    "reference_file_hf_uris",
    "deliverable_files",
    "deliverable_file_urls",
    "deliverable_file_hf_uris",
    "rubric_pretty",
    "rubric_json",
];
const AGENT_INSTRUCTIONS: &str = "Complete the professional task in the writable workspace. Create every requested final deliverable as a file in the workspace; the evaluator scores those files, not a response that only describes their contents. Use clear descriptive filenames and do not write outputs inside reference_files.";

/// OpenAI's public GDPval task, workspace-artifact, and rubric release.
#[derive(Clone, Debug)]
pub struct Gdpval {
    source: PathBuf,
    revision: String,
    environment: Environment,
    harness: PathBuf,
    task_ids: Option<BTreeSet<String>>,
}

impl Gdpval {
    /// Creates an importer for one pinned public GDPval repository snapshot.
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
            task_ids: None,
        }
    }

    /// Restricts normalization to task IDs selected by the resolved profile.
    #[must_use]
    pub fn tasks(mut self, task_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.task_ids = Some(task_ids.into_iter().map(Into::into).collect());
        self
    }
}

impl DatasetImporter for Gdpval {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let parquet = self.source.join(PARQUET_PATH);
        let cases = read_cases(&parquet)?;
        validate_release(&parquet, &cases)?;
        let cases = cases
            .into_iter()
            .filter(|case| {
                self.task_ids
                    .as_ref()
                    .is_none_or(|task_ids| task_ids.contains(&case.task_id))
            })
            .collect::<Vec<_>>();
        if cases.is_empty() {
            return Err(ImportError::Invalid(
                "GDPval profile selection contains no task from the pinned release".to_owned(),
            ));
        }

        let mut source_values = Vec::with_capacity(
            1 + HARNESS_FILES.len() + EXPECTED_REFERENCE_FILES + EXPECTED_DELIVERABLE_FILES,
        );
        source_values.push(sha256_file(&parquet)?);
        for relative in HARNESS_FILES {
            source_values.push(sha256_file(&self.harness.join(relative))?);
        }
        let harness = Harness::directory(&self.harness)?;
        let mut planned = Vec::with_capacity(cases.len());
        for case in cases {
            let metadata = case.verifier_metadata();
            let metadata = serde_json::to_vec(&metadata).map_err(|source| ImportError::Json {
                path: parquet.clone(),
                source,
            })?;
            let mut task = CasePlan::hermetic(
                &case.task_id,
                &case.prompt,
                self.environment.clone(),
                harness.clone(),
            )?
            .benchmark_prompt_chars(character_count(&case.prompt, &case.task_id)?)
            .benchmark_case_type(format!("{}/{}", case.sector, case.occupation))
            .instructions(AGENT_INSTRUCTIONS)
            .output(TaskOutput::Workspace)
            .resources(Resources {
                cpus: 4,
                memory_mb: 8_192,
                storage_mb: 16_384,
                gpus: 0,
            })
            .timeouts(Duration::from_secs(7_200), Duration::from_secs(1_800))
            .allow_internet(true)
            .verifier_allow_internet(true)
            .harness_file("case.json", metadata, 0o600)?;

            for asset in &case.reference_files {
                let source = self.source.join(&asset.path);
                source_values.push(sha256_file(&source)?);
                task = task.environment_file_from(
                    Path::new("reference_files").join(asset.basename()?),
                    source,
                    0o644,
                )?;
            }
            for asset in &case.deliverable_files {
                let source = self.source.join(&asset.path);
                source_values.push(sha256_file(&source)?);
                task = task.harness_file_from(
                    Path::new("expert").join(asset.basename()?),
                    source,
                    0o600,
                )?;
            }
            planned.push(task);
        }

        let source = SourceIdentity::new(
            "openai-gdpval-public",
            &self.revision,
            sha256_values(source_values.iter().map(String::as_bytes)),
        )?;
        let mut plan = DatasetPlan::new("gdpval", source)?;
        for case in planned {
            plan = plan.case(case);
        }
        Ok(plan)
    }
}

#[derive(Clone)]
struct GdpvalCase {
    task_id: String,
    sector: String,
    occupation: String,
    prompt: String,
    reference_files: Vec<Asset>,
    deliverable_files: Vec<Asset>,
    rubric_pretty: String,
    rubric_items: Vec<RubricItem>,
}

#[derive(Clone, Serialize)]
struct Asset {
    path: PathBuf,
    url: String,
    hf_uri: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RubricItem {
    score: i64,
    criterion: String,
    required: Option<()>,
    rubric_item_id: String,
    author_type: AuthorType,
    tags: Vec<String>,
    read_only: Option<()>,
    #[serde(default)]
    form_content: Option<()>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum AuthorType {
    Human,
}

#[derive(Serialize)]
struct VerifierCase<'a> {
    task_id: &'a str,
    sector: &'a str,
    occupation: &'a str,
    prompt: &'a str,
    deliverables: &'a [Asset],
    rubric_pretty: &'a str,
    rubric_items: &'a [RubricItem],
    scoring: ScoringContract,
}

#[derive(Serialize)]
struct ScoringContract {
    reproduction: &'static str,
    grader_model: &'static str,
    grader_reasoning_effort: &'static str,
    presentation_orders: u8,
}

impl GdpvalCase {
    fn from_row(path: &Path, index: usize, row: &Row) -> Result<Self, ImportError> {
        let columns = row
            .get_column_iter()
            .map(|(name, _)| name.as_str())
            .collect::<BTreeSet<_>>();
        let expected = COLUMNS.into_iter().collect::<BTreeSet<_>>();
        if columns != expected {
            return Err(ImportError::Invalid(format!(
                "{} row {} has GDPval columns {columns:?}, expected {expected:?}",
                path.display(),
                index + 1
            )));
        }
        let task_id = text(path, index, row, "task_id")?;
        let sector = text(path, index, row, "sector")?;
        let occupation = text(path, index, row, "occupation")?;
        let prompt = text(path, index, row, "prompt")?;
        let reference_files = assets(path, index, row, "reference")?;
        let deliverable_files = assets(path, index, row, "deliverable")?;
        let rubric_pretty = text(path, index, row, "rubric_pretty")?;
        let rubric_json = text(path, index, row, "rubric_json")?;
        let rubric_items =
            serde_json::from_str::<Vec<RubricItem>>(&rubric_json).map_err(|error| {
                ImportError::Invalid(format!(
                    "{} row {} has invalid rubric_json: {error}",
                    path.display(),
                    index + 1
                ))
            })?;
        let case = Self {
            task_id,
            sector,
            occupation,
            prompt,
            reference_files,
            deliverable_files,
            rubric_pretty,
            rubric_items,
        };
        case.validate(path, index)?;
        Ok(case)
    }

    fn validate(&self, path: &Path, index: usize) -> Result<(), ImportError> {
        if !is_uuid(&self.task_id) {
            return invalid_case(path, index, &self.task_id, "invalid task UUID");
        }
        if self.sector.trim().is_empty()
            || self.occupation.trim().is_empty()
            || self.prompt.trim().is_empty()
            || self.rubric_pretty.trim().is_empty()
        {
            return invalid_case(path, index, &self.task_id, "empty task metadata");
        }
        let mut basenames = HashSet::new();
        for asset in &self.reference_files {
            validate_asset(asset, "reference_files", &self.task_id)?;
            if !basenames.insert(asset.basename()?.to_owned()) {
                return invalid_case(path, index, &self.task_id, "duplicate reference basename");
            }
        }
        basenames.clear();
        for asset in &self.deliverable_files {
            validate_asset(asset, "deliverable_files", &self.task_id)?;
            if !basenames.insert(asset.basename()?.to_owned()) {
                return invalid_case(path, index, &self.task_id, "duplicate deliverable basename");
            }
        }
        if self.rubric_items.is_empty()
            || !self.rubric_items.iter().any(|item| item.score > 0)
            || self.rubric_items.iter().any(|item| {
                item.score == 0
                    || item.criterion.trim().is_empty()
                    || !is_uuid(&item.rubric_item_id)
            })
        {
            return invalid_case(path, index, &self.task_id, "invalid rubric");
        }
        Ok(())
    }

    fn verifier_metadata(&self) -> VerifierCase<'_> {
        VerifierCase {
            task_id: &self.task_id,
            sector: &self.sector,
            occupation: &self.occupation,
            prompt: &self.prompt,
            deliverables: &self.deliverable_files,
            rubric_pretty: &self.rubric_pretty,
            rubric_items: &self.rubric_items,
            scoring: ScoringContract {
                reproduction: "nanocodex-public-gdpval-pairwise-v1",
                grader_model: "gpt-5.6-sol",
                grader_reasoning_effort: "low",
                presentation_orders: 2,
            },
        }
    }
}

impl Asset {
    fn basename(&self) -> Result<&std::ffi::OsStr, ImportError> {
        self.path.file_name().ok_or_else(|| {
            ImportError::Invalid(format!(
                "GDPval asset has no basename: {}",
                self.path.display()
            ))
        })
    }
}

fn read_cases(path: &Path) -> Result<Vec<GdpvalCase>, ImportError> {
    let file = File::open(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = SerializedFileReader::new(file).map_err(|error| {
        ImportError::Invalid(format!("failed to open {}: {error}", path.display()))
    })?;
    reader
        .get_row_iter(None)
        .map_err(|error| {
            ImportError::Invalid(format!("failed to read {}: {error}", path.display()))
        })?
        .enumerate()
        .map(|(index, row)| {
            let row = row.map_err(|error| {
                ImportError::Invalid(format!(
                    "failed to decode {} row {}: {error}",
                    path.display(),
                    index + 1
                ))
            })?;
            GdpvalCase::from_row(path, index, &row)
        })
        .collect()
}

pub(crate) fn asset_paths(
    path: &Path,
    task_ids: Option<&BTreeSet<String>>,
) -> Result<Vec<PathBuf>, ImportError> {
    let cases = read_cases(path)?;
    validate_release(path, &cases)?;
    Ok(cases
        .into_iter()
        .filter(|case| task_ids.is_none_or(|task_ids| task_ids.contains(&case.task_id)))
        .flat_map(|case| {
            case.reference_files
                .into_iter()
                .chain(case.deliverable_files)
                .map(|asset| asset.path)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn validate_release(path: &Path, cases: &[GdpvalCase]) -> Result<(), ImportError> {
    let ids = cases
        .iter()
        .map(|case| case.task_id.as_str())
        .collect::<HashSet<_>>();
    let sectors = cases
        .iter()
        .map(|case| case.sector.as_str())
        .collect::<HashSet<_>>();
    let occupations = cases
        .iter()
        .map(|case| case.occupation.as_str())
        .collect::<HashSet<_>>();
    let rubrics = cases
        .iter()
        .flat_map(|case| &case.rubric_items)
        .collect::<Vec<_>>();
    let rubric_ids = rubrics
        .iter()
        .map(|rubric| rubric.rubric_item_id.as_str())
        .collect::<HashSet<_>>();
    let references = cases
        .iter()
        .flat_map(|case| case.reference_files.iter().map(|asset| &asset.path))
        .collect::<HashSet<_>>();
    let deliverables = cases
        .iter()
        .flat_map(|case| case.deliverable_files.iter().map(|asset| &asset.path))
        .collect::<HashSet<_>>();
    if cases.len() != EXPECTED_CASES
        || ids.len() != EXPECTED_CASES
        || sectors.len() != EXPECTED_SECTORS
        || occupations.len() != EXPECTED_OCCUPATIONS
        || rubrics.len() != EXPECTED_RUBRICS
        || rubric_ids.len() != EXPECTED_RUBRICS
        || references.len() != EXPECTED_REFERENCE_FILES
        || deliverables.len() != EXPECTED_DELIVERABLE_FILES
    {
        return Err(ImportError::Invalid(format!(
            "{} is not the pinned GDPval release: cases={}/{EXPECTED_CASES}, unique_ids={}, sectors={}/{EXPECTED_SECTORS}, occupations={}/{EXPECTED_OCCUPATIONS}, rubrics={}/{EXPECTED_RUBRICS}, unique_rubrics={}, references={}/{EXPECTED_REFERENCE_FILES}, deliverables={}/{EXPECTED_DELIVERABLE_FILES}",
            path.display(),
            cases.len(),
            ids.len(),
            sectors.len(),
            occupations.len(),
            rubrics.len(),
            rubric_ids.len(),
            references.len(),
            deliverables.len(),
        )));
    }
    Ok(())
}

fn text(path: &Path, row_number: usize, row: &Row, name: &str) -> Result<String, ImportError> {
    let index = field(path, row_number, row, name)?;
    row.get_string(index).cloned().map_err(|error| {
        ImportError::Invalid(format!(
            "{} row {} {name} is not text: {error}",
            path.display(),
            row_number + 1
        ))
    })
}

fn strings(
    path: &Path,
    row_number: usize,
    row: &Row,
    name: &str,
) -> Result<Vec<String>, ImportError> {
    let index = field(path, row_number, row, name)?;
    let values = row.get_list(index).map_err(|error| {
        ImportError::Invalid(format!(
            "{} row {} {name} is not a list: {error}",
            path.display(),
            row_number + 1
        ))
    })?;
    (0..values.len())
        .map(|item| {
            values.get_string(item).cloned().map_err(|error| {
                ImportError::Invalid(format!(
                    "{} row {} {name} contains non-text: {error}",
                    path.display(),
                    row_number + 1
                ))
            })
        })
        .collect()
}

fn assets(
    path: &Path,
    row_number: usize,
    row: &Row,
    kind: &str,
) -> Result<Vec<Asset>, ImportError> {
    let paths = strings(path, row_number, row, &format!("{kind}_files"))?;
    let urls = strings(path, row_number, row, &format!("{kind}_file_urls"))?;
    let uris = strings(path, row_number, row, &format!("{kind}_file_hf_uris"))?;
    if paths.len() != urls.len() || paths.len() != uris.len() {
        return Err(ImportError::Invalid(format!(
            "{} row {} {kind} asset arrays have different lengths",
            path.display(),
            row_number + 1
        )));
    }
    Ok(paths
        .into_iter()
        .zip(urls)
        .zip(uris)
        .map(|((path, url), hf_uri)| Asset {
            path: PathBuf::from(path),
            url,
            hf_uri,
        })
        .collect())
}

fn field(path: &Path, row_number: usize, row: &Row, name: &str) -> Result<usize, ImportError> {
    row.get_column_iter()
        .position(|(candidate, _)| candidate == name)
        .ok_or_else(|| {
            ImportError::Invalid(format!(
                "{} row {} has no {name:?} column",
                path.display(),
                row_number + 1
            ))
        })
}

fn validate_asset(asset: &Asset, prefix: &str, task_id: &str) -> Result<(), ImportError> {
    let components = asset.path.components().collect::<Vec<_>>();
    let valid = matches!(components.as_slice(), [Component::Normal(root), Component::Normal(hash), Component::Normal(name)]
        if *root == std::ffi::OsStr::new(prefix)
            && hash.to_str().is_some_and(is_hex_id)
            && !name.is_empty());
    if !valid
        || asset.url.trim().is_empty()
        || asset.hf_uri.trim().is_empty()
        || !asset.url.contains("huggingface.co/datasets/openai/gdpval/")
        || !asset
            .hf_uri
            .starts_with("hf://datasets/openai/gdpval@main/")
    {
        return Err(ImportError::Invalid(format!(
            "GDPval task {task_id} has invalid {prefix} asset {}",
            asset.path.display()
        )));
    }
    Ok(())
}

fn is_hex_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
            }
        })
}

fn character_count(value: &str, task_id: &str) -> Result<u64, ImportError> {
    u64::try_from(value.chars().count())
        .map_err(|_| ImportError::Invalid(format!("GDPval task {task_id} prompt length overflows")))
}

fn invalid_case<T>(
    path: &Path,
    index: usize,
    task_id: &str,
    message: &str,
) -> Result<T, ImportError> {
    Err(ImportError::Invalid(format!(
        "{} row {} GDPval task {task_id:?} has {message}",
        path.display(),
        index + 1
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(prefix: &str, name: &str) -> Asset {
        Asset {
            path: PathBuf::from(format!("{prefix}/0123456789abcdef0123456789abcdef/{name}")),
            url: format!(
                "https://huggingface.co/datasets/openai/gdpval/resolve/main/{prefix}/{name}"
            ),
            hf_uri: format!("hf://datasets/openai/gdpval@main/{prefix}/{name}"),
        }
    }

    fn case() -> GdpvalCase {
        GdpvalCase {
            task_id: "854f3814-681c-4950-91ac-55b0db0e3781".to_owned(),
            sector: "Professional Services".to_owned(),
            occupation: "Software Developers".to_owned(),
            prompt: "Produce both requested files.".to_owned(),
            reference_files: Vec::new(),
            deliverable_files: vec![asset("deliverable_files", "answer.md")],
            rubric_pretty: "Complete and correct.".to_owned(),
            rubric_items: vec![RubricItem {
                score: 5,
                criterion: "The answer is complete.".to_owned(),
                required: None,
                rubric_item_id: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
                author_type: AuthorType::Human,
                tags: Vec::new(),
                read_only: None,
                form_content: None,
            }],
        }
    }

    #[test]
    fn keeps_expert_and_rubrics_in_verifier_metadata() {
        let case = case();
        case.validate(Path::new("fixture.parquet"), 0).unwrap();
        let metadata = serde_json::to_string(&case.verifier_metadata()).unwrap();

        assert!(metadata.contains("deliverable_files"));
        assert!(metadata.contains("Complete and correct"));
        assert!(!case.prompt.contains("Complete and correct"));
    }

    #[test]
    fn rejects_asset_traversal() {
        let mut case = case();
        case.deliverable_files[0].path = PathBuf::from("deliverable_files/../answer.md");

        let error = case.validate(Path::new("fixture.parquet"), 0).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid deliverable_files asset")
        );
    }

    #[test]
    fn rejects_rubric_without_positive_points() {
        let mut case = case();
        case.rubric_items[0].score = -5;

        let error = case.validate(Path::new("fixture.parquet"), 0).unwrap_err();

        assert!(error.to_string().contains("invalid rubric"));
    }
}
