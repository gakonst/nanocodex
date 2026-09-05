//! Profile-level durable execution API.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    path::{Path, PathBuf},
};

use crate::{
    Task,
    profile::{EvaluationManifest, ResolvedFamily, ResolvedHarness, ResolvedProfile, ResolvedTask},
    workset::{
        BeginTask, RecentAttemptCounts, TaskClaim, Workset, WorksetBusy, WorksetError,
        WorksetFamily, WorksetObserver, WorksetStatus, WorksetTask,
    },
};
use nanocodex_oai_api::{Model, Thinking};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

const LEDGER_FILE: &str = "state.sqlite3";

/// One named benchmark whose latest generation lives in SQLite.
#[derive(Clone, Debug)]
pub struct Evaluation {
    name: String,
    state_directory: PathBuf,
    config: PathBuf,
}

/// Persistent read-only observer for one initialized profile ledger.
///
/// The observer never initializes or mutates SQLite. Its cheap refresh probe
/// uses SQLite's commit counter and only rebuilds a status snapshot when the
/// ledger changed or an owner process died.
pub struct EvaluationObserver {
    workset: WorksetObserver,
}

/// Optional knobs selecting one exact family already present in a profile.
#[derive(Clone, Debug)]
pub struct EvaluationSelector {
    task: String,
    harness: Option<String>,
    model: Option<Model>,
    thinking: Option<Thinking>,
}

/// One concrete task treatment to materialize in SQLite.
#[derive(Clone, Debug)]
pub struct EvaluationWork {
    selector: String,
    task: Task,
    harness: String,
    model: Model,
    thinking: Thinking,
    web_search: bool,
    trials: u16,
}

/// The next durable action for one profile family.
#[derive(Debug)]
pub enum EvaluationClaim {
    /// Execute one pre-materialized task row.
    Run(CoordinateClaim),
    /// Matching work exists but another process currently owns it.
    Busy(EvaluationBusy),
    /// Every trial in the selected family has an accepted result.
    Complete,
}

/// Owned execution of one pre-materialized task row.
#[derive(Debug)]
pub struct CoordinateClaim {
    workset: Workset,
    claim: TaskClaim,
    task: Task,
    task_selector: String,
    treatment: EvaluationTreatment,
    web_search: bool,
    harness: Option<ResolvedHarness>,
    harnesses: Vec<ResolvedHarness>,
    output_directory: PathBuf,
    finished: bool,
}

/// Semantic knobs fixed by one profile family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvaluationTreatment {
    /// Built-in or configured harness used for this coordinate.
    pub harness: String,
    /// Model fixed by the profile.
    pub model: Model,
    /// Reasoning effort fixed by the profile.
    #[serde(serialize_with = "crate::profile::serialize_one_thinking")]
    pub thinking: Thinking,
    /// Whether model-facing web search is enabled for this row.
    pub web_search: bool,
}

/// Temporary inability to claim the selected family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvaluationBusy {
    /// Stable machine-readable reason.
    pub reason: &'static str,
    /// Suggested delay before retrying.
    pub retry_after_ms: u64,
}

/// Complete durable status of one immutable profile revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvaluationStatus {
    /// Selected profile name.
    pub profile: String,
    /// Digest of the profile, tasks, harness, and treatments.
    pub digest: String,
    /// Pre-materialized task-row counts.
    pub tasks: EvaluationCounts,
    /// Stable names of workers that currently own running rows.
    pub workers: Vec<String>,
    /// Terminal attempt outcomes recorded during the last five minutes.
    pub recent_attempts: RecentAttemptCounts,
    /// Status grouped by exact semantic treatment.
    pub families: Vec<EvaluationFamilyStatus>,
}

/// Counts for one durable work state machine.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct EvaluationCounts {
    /// Work available to claim.
    pub unclaimed: i64,
    /// Work with a live owner.
    pub running: i64,
    /// Successfully completed work.
    pub success: i64,
    /// Failed work.
    pub failed: i64,
}

impl EvaluationCounts {
    /// Total number of pre-materialized task rows.
    #[must_use]
    pub const fn total(&self) -> i64 {
        self.unclaimed + self.running + self.success + self.failed
    }

    /// Number of terminal task rows.
    #[must_use]
    pub const fn finished(&self) -> i64 {
        self.success + self.failed
    }
}

/// Durable status of one exact profile treatment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvaluationFamilyStatus {
    /// Stable family identity.
    pub id: String,
    /// Profile-visible task selector.
    pub task: String,
    /// Semantic treatment fixed by the profile.
    pub treatment: EvaluationTreatment,
    /// Desired fungible trial count.
    pub desired: i64,
    /// Rows available to claim.
    pub unclaimed: i64,
    /// Rows with live owners.
    pub running: i64,
    /// Successful rows.
    pub success: i64,
    /// Failed rows.
    pub failed: i64,
}

/// Profile resolution, selection, or durable-ledger failure.
#[derive(Debug)]
pub struct EvaluationError {
    source: Box<dyn Error + Send + Sync>,
}

impl Evaluation {
    /// Reads the adapter-owned benchmark selectors requested by a TOML profile.
    pub fn profile_benchmarks(
        config: impl AsRef<Path>,
        recipe: Option<&str>,
    ) -> Result<Vec<String>, EvaluationError> {
        EvaluationManifest::load_benchmarks(config, recipe).map_err(error)
    }

    /// Appends concrete work, creating the benchmark when it does not exist.
    pub fn add(
        state_directory: impl Into<PathBuf>,
        name: &str,
        work: &[EvaluationWork],
        new_generation: bool,
    ) -> Result<(), EvaluationError> {
        if work.is_empty() {
            return Err(error(std::io::Error::other(
                "at least one concrete evaluation treatment is required",
            )));
        }
        let path = state_directory.into().join(LEDGER_FILE);
        let workset = if new_generation {
            Workset::create(&path, name)
        } else {
            match Workset::open(&path, name) {
                Ok(workset) => Ok(workset),
                Err(WorksetError::UnknownProfile(_)) => Workset::create(&path, name),
                Err(source) => Err(source),
            }
        }
        .map_err(error)?;
        let mut tasks = std::collections::BTreeMap::new();
        let mut families = Vec::with_capacity(work.len());
        for item in work {
            if item.trials == 0 {
                return Err(error(std::io::Error::other(
                    "evaluation treatments must request at least one trial",
                )));
            }
            tasks
                .entry(item.selector.clone())
                .or_insert_with(|| WorksetTask {
                    selector: item.selector.clone(),
                    name: item.task.name().to_owned(),
                    root: item.task.root().to_path_buf(),
                    digest: item.task.package_digest().to_owned(),
                });
            let family = item.family();
            families.push(WorksetFamily {
                key: family.key.clone(),
                task_selector: family.task.clone(),
                harness: family.harness.clone(),
                model: family.model.as_str().to_owned(),
                thinking: family.thinking.as_str().to_owned(),
                web_search: item.web_search,
                trials: item.trials,
            });
        }
        workset
            .append(&tasks.into_values().collect::<Vec<_>>(), &families)
            .map_err(error)
    }

    /// Expands one TOML profile recipe into explicit durable rows.
    pub fn add_profile(
        config: impl AsRef<Path>,
        recipe: Option<&str>,
        state_directory: impl Into<PathBuf>,
        name: &str,
        new_generation: bool,
    ) -> Result<(), EvaluationError> {
        let recipe = EvaluationManifest::load_profile(config, recipe).map_err(error)?;
        Self::add_resolved_profile(recipe, state_directory, name, new_generation)
    }

    /// Expands a TOML profile plus adapter-resolved tasks into durable rows.
    pub fn add_profile_with_tasks(
        config: impl AsRef<Path>,
        recipe: Option<&str>,
        benchmark_tasks: Vec<ResolvedTask>,
        state_directory: impl Into<PathBuf>,
        name: &str,
        new_generation: bool,
    ) -> Result<(), EvaluationError> {
        let recipe = EvaluationManifest::load_profile_with_tasks(config, recipe, benchmark_tasks)
            .map_err(error)?;
        Self::add_resolved_profile(recipe, state_directory, name, new_generation)
    }

    fn add_resolved_profile(
        recipe: ResolvedProfile,
        state_directory: impl Into<PathBuf>,
        name: &str,
        new_generation: bool,
    ) -> Result<(), EvaluationError> {
        let mut work = Vec::with_capacity(recipe.families.len());
        for task in &recipe.tasks {
            for family in recipe
                .families
                .iter()
                .filter(|family| family.task == task.selector)
            {
                work.push(
                    EvaluationWork::new(&family.task, task.task.clone())
                        .harness(&family.harness)
                        .model(family.model)
                        .thinking(family.thinking)
                        .web_search(recipe.web_search)
                        .trials(recipe.trials),
                );
            }
        }
        Self::add(state_directory, name, &work, new_generation)
    }

    /// Resolves one runtime harness helper from current local configuration.
    pub fn resolve_harness(
        config: impl AsRef<Path>,
        harness: &str,
    ) -> Result<Option<ResolvedHarness>, EvaluationError> {
        EvaluationManifest::load_harness(config, harness).map_err(error)
    }

    /// Opens the newest SQLite generation. This never creates task rows.
    pub fn open(
        config: impl AsRef<Path>,
        profile: Option<&str>,
        state_directory: impl Into<PathBuf>,
    ) -> Result<Self, EvaluationError> {
        let state_directory = state_directory.into();
        let name = profile.ok_or_else(|| {
            error(std::io::Error::other(
                "a benchmark name is required to select SQLite work",
            ))
        })?;
        Workset::open(state_directory.join(LEDGER_FILE), name).map_err(error)?;
        Ok(Self {
            name: name.to_owned(),
            state_directory,
            config: config.as_ref().to_path_buf(),
        })
    }

    /// Selected profile name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn state_directory(&self) -> &Path {
        &self.state_directory
    }

    /// Reads a structured snapshot from SQLite.
    pub fn status(&self) -> Result<EvaluationStatus, EvaluationError> {
        observed_status(self.workset()?.status().map_err(error)?)
    }

    /// Atomically claims one pre-materialized row from an exact family.
    pub fn claim(&self, selector: &EvaluationSelector) -> Result<EvaluationClaim, EvaluationError> {
        self.claim_for_worker(selector, "local")
    }

    /// Atomically claims the next unclaimed row in the benchmark.
    pub fn claim_next(&self) -> Result<EvaluationClaim, EvaluationError> {
        self.claim_next_for_worker("local")
    }

    /// Claims the next row for a named worker.
    pub(crate) fn claim_next_for_worker(
        &self,
        worker: &str,
    ) -> Result<EvaluationClaim, EvaluationError> {
        let workset = self.workset()?;
        match workset.begin_next_for_worker(worker).map_err(error)? {
            BeginTask::Run(claim) => self.running_claim(workset, claim).map(EvaluationClaim::Run),
            BeginTask::Busy(busy) => Ok(EvaluationClaim::Busy(busy.into())),
            BeginTask::Complete => Ok(EvaluationClaim::Complete),
        }
    }

    /// Claims one family for a named worker.
    pub(crate) fn claim_for_worker(
        &self,
        selector: &EvaluationSelector,
        worker: &str,
    ) -> Result<EvaluationClaim, EvaluationError> {
        let workset = self.workset()?;
        let family = self.resolve_family(&workset, selector)?;
        self.claim_resolved(workset, &family.key, worker)
    }

    pub(crate) fn recover_running(
        &self,
    ) -> Result<Vec<(CoordinateClaim, String)>, EvaluationError> {
        let workset = self.workset()?;
        workset
            .recover_running()
            .map_err(error)?
            .into_iter()
            .map(|(claim, worker)| {
                self.running_claim(workset.clone(), claim)
                    .map(|claim| (claim, worker))
            })
            .collect()
    }

    fn resolve_family(
        &self,
        workset: &Workset,
        selector: &EvaluationSelector,
    ) -> Result<WorksetFamily, EvaluationError> {
        let Some((_, families)) = workset.selected_definition(&selector.task).map_err(error)?
        else {
            return Err(error(std::io::Error::other(format!(
                "task `{}` is not part of benchmark `{}`",
                selector.task, self.name
            ))));
        };
        let harness = selector.harness.as_deref().unwrap_or("nanocodex");
        let mut matching = Vec::new();
        for family in families {
            if family.harness == harness
                && selector
                    .model
                    .is_none_or(|model| family.model == model.as_str())
                && selector
                    .thinking
                    .is_none_or(|thinking| family.thinking == thinking.as_str())
            {
                matching.push(family);
            }
        }
        match matching.len() {
            1 => Ok(matching.remove(0)),
            0 => Err(error(std::io::Error::other("no matching SQLite treatment"))),
            _ => Err(error(std::io::Error::other(
                "multiple SQLite treatments match; select model and thinking",
            ))),
        }
    }

    fn claim_resolved(
        &self,
        workset: Workset,
        family_key: &str,
        worker: &str,
    ) -> Result<EvaluationClaim, EvaluationError> {
        match workset
            .begin_for_worker(family_key, worker)
            .map_err(error)?
        {
            BeginTask::Run(claim) => self.running_claim(workset, claim).map(EvaluationClaim::Run),
            BeginTask::Busy(busy) => Ok(EvaluationClaim::Busy(busy.into())),
            BeginTask::Complete => Ok(EvaluationClaim::Complete),
        }
    }

    fn running_claim(
        &self,
        workset: Workset,
        claim: TaskClaim,
    ) -> Result<CoordinateClaim, EvaluationError> {
        let Some((retained_task, family)) = workset
            .family_definition(&claim.family_key)
            .map_err(error)?
        else {
            return Err(error(std::io::Error::other(format!(
                "SQLite contains unknown family `{}`",
                claim.family_key
            ))));
        };
        let task = Task::load(&retained_task.root).map_err(error)?;
        if task.name() != retained_task.name || task.package_digest() != retained_task.digest {
            return Err(error(std::io::Error::other(format!(
                "retained task `{}` no longer matches SQLite content digest {}",
                retained_task.selector, retained_task.digest
            ))));
        }
        let treatment = family_treatment(&family)?;
        let harness =
            EvaluationManifest::load_harness(&self.config, &treatment.harness).map_err(error)?;
        let harnesses = harness.iter().cloned().collect();
        let output_directory = coordinate_output(
            &self.state_directory,
            workset.generation(),
            &claim.family_key,
            claim.repetition,
            claim.id(),
        );
        Ok(CoordinateClaim {
            workset,
            claim,
            task,
            task_selector: retained_task.selector,
            web_search: treatment.web_search,
            treatment,
            harness,
            harnesses,
            output_directory,
            finished: false,
        })
    }

    fn workset(&self) -> Result<Workset, EvaluationError> {
        Workset::open(self.state_directory.join(LEDGER_FILE), &self.name).map_err(error)
    }
}

impl EvaluationWork {
    /// Creates one built-in Nanocodex treatment.
    #[must_use]
    pub fn new(selector: impl Into<String>, task: Task) -> Self {
        Self {
            selector: selector.into(),
            task,
            harness: "nanocodex".to_owned(),
            model: Model::default(),
            thinking: Thinking::default(),
            web_search: false,
            trials: 1,
        }
    }

    /// Sets the runtime harness name retained in SQLite.
    #[must_use]
    pub fn harness(mut self, harness: impl Into<String>) -> Self {
        self.harness = harness.into();
        self
    }

    /// Sets the model retained in SQLite.
    #[must_use]
    pub const fn model(mut self, model: Model) -> Self {
        self.model = model;
        self
    }

    /// Sets reasoning effort retained in SQLite.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = thinking;
        self
    }

    /// Sets model-facing web-search policy.
    #[must_use]
    pub const fn web_search(mut self, web_search: bool) -> Self {
        self.web_search = web_search;
        self
    }

    /// Sets the number of rows pre-materialized for this treatment.
    #[must_use]
    pub const fn trials(mut self, trials: u16) -> Self {
        self.trials = trials;
        self
    }

    fn family(&self) -> ResolvedFamily {
        ResolvedFamily {
            key: format!(
                "{}|{}|{}|{}{}",
                self.selector,
                self.harness,
                self.model.as_str(),
                self.thinking.as_str(),
                if self.web_search { "|web-search" } else { "" }
            ),
            task: self.selector.clone(),
            harness: self.harness.clone(),
            model: self.model,
            thinking: self.thinking,
        }
    }
}

impl EvaluationObserver {
    /// Opens an existing profile ledger without creating files or taking a
    /// write-capable SQLite connection.
    pub fn open(state_directory: impl AsRef<Path>, profile: &str) -> Result<Self, EvaluationError> {
        let path = state_directory.as_ref().join(LEDGER_FILE);
        let workset = WorksetObserver::open(&path, profile).map_err(error)?;
        Ok(Self { workset })
    }

    /// Forces a complete, internally consistent status snapshot.
    pub fn snapshot(&mut self) -> Result<EvaluationStatus, EvaluationError> {
        observed_status(self.workset.snapshot().map_err(error)?)
    }

    /// Returns a new snapshot after a commit or owner-process death.
    pub fn refresh(&mut self) -> Result<Option<EvaluationStatus>, EvaluationError> {
        self.workset
            .refresh()
            .map_err(error)?
            .map(observed_status)
            .transpose()
    }
}

fn observed_status(status: WorksetStatus) -> Result<EvaluationStatus, EvaluationError> {
    let families = status
        .families
        .into_iter()
        .map(|family| {
            let treatment = normalized_treatment(
                family.harness,
                family.model,
                family.thinking,
                family.web_search,
            )?;
            Ok(EvaluationFamilyStatus {
                id: family.key,
                task: family.task,
                treatment,
                desired: family.desired,
                unclaimed: family.unclaimed,
                running: family.running,
                success: family.success,
                failed: family.failed,
            })
        })
        .collect::<Result<Vec<_>, EvaluationError>>()?;
    Ok(EvaluationStatus {
        profile: status.profile,
        digest: status.digest,
        tasks: EvaluationCounts {
            unclaimed: status.tasks.unclaimed,
            running: status.tasks.running,
            success: status.tasks.success,
            failed: status.tasks.failed,
        },
        workers: status.workers,
        recent_attempts: status.recent_attempts,
        families,
    })
}

fn family_treatment(family: &WorksetFamily) -> Result<EvaluationTreatment, EvaluationError> {
    normalized_treatment(
        family.harness.clone(),
        family.model.clone(),
        family.thinking.clone(),
        family.web_search,
    )
}

fn normalized_treatment(
    harness: String,
    model: String,
    thinking: String,
    web_search: bool,
) -> Result<EvaluationTreatment, EvaluationError> {
    let model = model
        .parse()
        .map_err(|message: String| error(std::io::Error::other(message)))?;
    let thinking = thinking
        .parse()
        .map_err(|message: String| error(std::io::Error::other(message)))?;
    Ok(EvaluationTreatment {
        harness,
        model,
        thinking,
        web_search,
    })
}

impl EvaluationSelector {
    /// Selects a task from the closed profile.
    #[must_use]
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            harness: None,
            model: None,
            thinking: None,
        }
    }

    /// Selects one configured external harness. Omission selects Nanocodex.
    #[must_use]
    pub fn harness(mut self, harness: Option<impl Into<String>>) -> Self {
        self.harness = harness.map(Into::into);
        self
    }

    /// Narrows the task to one profile-owned model treatment.
    #[must_use]
    pub const fn model(mut self, model: Option<Model>) -> Self {
        self.model = model;
        self
    }

    /// Narrows the task to one profile-owned reasoning treatment.
    #[must_use]
    pub const fn thinking(mut self, thinking: Option<Thinking>) -> Self {
        self.thinking = thinking;
        self
    }

    pub(crate) fn task(&self) -> &str {
        &self.task
    }

    pub(crate) fn harness_name(&self) -> Option<&str> {
        self.harness.as_deref()
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.map(Model::as_str)
    }

    pub(crate) fn thinking_name(&self) -> Option<&str> {
        self.thinking.map(Thinking::as_str)
    }
}

impl CoordinateClaim {
    pub(crate) fn id(&self) -> &str {
        self.claim.id()
    }

    /// Immutable task package for this trial.
    #[must_use]
    pub const fn task(&self) -> &Task {
        &self.task
    }

    /// Benchmark-visible selector for this task row.
    #[must_use]
    pub fn task_selector(&self) -> &str {
        &self.task_selector
    }

    /// Stable family identity selected for this row.
    #[must_use]
    pub fn family_key(&self) -> &str {
        &self.claim.family_key
    }

    /// Semantic treatment fixed by the profile.
    #[must_use]
    pub const fn treatment(&self) -> &EvaluationTreatment {
        &self.treatment
    }

    /// Internal fungible repetition allocated by SQLite.
    #[must_use]
    pub const fn repetition(&self) -> u16 {
        self.claim.repetition
    }

    /// Whether model-facing web search is enabled by the profile.
    #[must_use]
    pub const fn web_search(&self) -> bool {
        self.web_search
    }

    /// Resolved configuration for the selected external harness.
    #[must_use]
    pub const fn harness(&self) -> Option<&ResolvedHarness> {
        self.harness.as_ref()
    }

    /// External harnesses installed into the immutable task image.
    #[must_use]
    pub fn harnesses(&self) -> &[ResolvedHarness] {
        &self.harnesses
    }

    /// Unique retained-artifact directory for this profile trial.
    #[must_use]
    pub fn output_directory(&self) -> &Path {
        &self.output_directory
    }

    /// Records successful execution if this claim still owns the row.
    pub fn succeed(mut self, evidence: &Path) -> Result<(), EvaluationError> {
        self.workset.succeed(&self.claim, evidence).map_err(error)?;
        self.finished = true;
        Ok(())
    }

    /// Records a verifier-failing execution if this claim still owns the row.
    pub fn fail(mut self, evidence: Option<&Path>, failure: &str) -> Result<(), EvaluationError> {
        self.workset
            .fail(&self.claim, evidence, failure)
            .map_err(error)?;
        self.finished = true;
        Ok(())
    }

    /// Retains an infrastructure failure and returns the coordinate to the claimable pool.
    pub fn retry(mut self, evidence: Option<&Path>, failure: &str) -> Result<(), EvaluationError> {
        self.workset
            .retry(&self.claim, evidence, failure)
            .map_err(error)?;
        self.finished = true;
        Ok(())
    }

    /// Releases an interrupted execution so another worker can claim its row.
    pub fn release(mut self, failure: &str) -> Result<(), EvaluationError> {
        self.workset.release(&self.claim, failure).map_err(error)?;
        self.finished = true;
        Ok(())
    }
}

impl From<&ResolvedFamily> for EvaluationTreatment {
    fn from(family: &ResolvedFamily) -> Self {
        Self {
            harness: family.harness.clone(),
            model: family.model,
            thinking: family.thinking,
            web_search: false,
        }
    }
}

impl From<WorksetBusy> for EvaluationBusy {
    fn from(busy: WorksetBusy) -> Self {
        Self {
            reason: busy.reason,
            retry_after_ms: busy.retry_after_ms,
        }
    }
}

impl Display for EvaluationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.source, formatter)
    }
}

impl Error for EvaluationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

fn error(source: impl Error + Send + Sync + 'static) -> EvaluationError {
    EvaluationError {
        source: Box::new(source),
    }
}

fn coordinate_output(
    state_directory: &Path,
    profile_digest: &str,
    family_key: &str,
    repetition: u16,
    claim_id: &str,
) -> PathBuf {
    let family_digest = hex::encode(Sha256::digest(family_key.as_bytes()));
    state_directory
        .join("artifacts")
        .join(profile_digest)
        .join(family_digest)
        .join(format!("k-{repetition}"))
        .join(claim_id)
}

impl Drop for CoordinateClaim {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self
                .workset
                .release(&self.claim, "claim dropped before recording an outcome");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    fn write_task(root: &Path) {
        let task = root.join("one");
        fs::create_dir_all(task.join("environment")).unwrap();
        fs::create_dir_all(task.join("tests")).unwrap();
        fs::write(
            task.join("task.toml"),
            r#"schema_version = "1.1"
[task]
name = "one"
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
"#,
        )
        .unwrap();
        fs::write(task.join("instruction.md"), "do it").unwrap();
        fs::write(task.join("environment/Dockerfile"), "FROM scratch").unwrap();
        fs::write(task.join("tests/test.sh"), "#!/bin/sh\n").unwrap();
    }

    #[tokio::test]
    async fn one_handle_owns_profile_expansion_claims_and_completion() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path());
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"[profiles.release]
tasks = ["one"]
trials = 2
model = ["sol"]
thinking = ["high"]
"#,
        )
        .unwrap();
        let state = directory.path().join("state");
        Evaluation::add_profile(&config, Some("release"), &state, "release", false).unwrap();
        let evaluation = Evaluation::open(&config, Some("release"), &state).unwrap();
        let selector = EvaluationSelector::new("one");

        let status = evaluation.status().unwrap();
        assert_eq!(status.tasks.unclaimed, 2);
        assert_eq!(status.families[0].desired, 2);
        assert_eq!(status.families[0].treatment.harness, "nanocodex");

        let EvaluationClaim::Run(coordinate) = evaluation.claim(&selector).unwrap() else {
            panic!("first claim should own a trial");
        };
        assert_eq!(coordinate.repetition(), 1);
        assert!(
            coordinate
                .output_directory()
                .starts_with(state.join("artifacts"))
        );
        coordinate.succeed(Path::new("accepted-result")).unwrap();

        let status = evaluation.status().unwrap();
        assert_eq!(status.tasks.success, 1);
        assert_eq!(status.tasks.unclaimed, 1);
    }

    #[tokio::test]
    async fn selectors_cannot_expand_the_profile() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path());
        let config = directory.path().join("nanocodex.toml");
        fs::write(
            &config,
            r#"[profiles.release]
tasks = ["one"]
trials = 1
"#,
        )
        .unwrap();
        Evaluation::add_profile(
            &config,
            Some("release"),
            directory.path().join("state"),
            "release",
            false,
        )
        .unwrap();
        let evaluation =
            Evaluation::open(&config, Some("release"), directory.path().join("state")).unwrap();

        let failure = evaluation
            .claim(&EvaluationSelector::new("outside"))
            .unwrap_err();
        assert!(failure.to_string().contains("is not part of benchmark"));
    }

    #[test]
    fn open_handle_reads_hot_appends_and_new_generations_from_sqlite() {
        let directory = tempfile::tempdir().unwrap();
        write_task(directory.path());
        let config = directory.path().join("nanocodex.toml");
        fs::write(&config, "").unwrap();
        let state = directory.path().join("state");
        let task = Task::load(directory.path().join("one")).unwrap();
        Evaluation::add(
            &state,
            "release",
            &[EvaluationWork::new("one", task.clone()).trials(1)],
            false,
        )
        .unwrap();
        let evaluation = Evaluation::open(&config, Some("release"), &state).unwrap();
        let first = evaluation.status().unwrap();
        assert_eq!(first.tasks.total(), 1);

        Evaluation::add(
            &state,
            "release",
            &[EvaluationWork::new("one", task.clone()).trials(3)],
            false,
        )
        .unwrap();
        assert_eq!(evaluation.status().unwrap().tasks.total(), 3);

        Evaluation::add(
            &state,
            "release",
            &[EvaluationWork::new("one", task).trials(2)],
            true,
        )
        .unwrap();
        let replaced = evaluation.status().unwrap();
        assert_ne!(replaced.digest, first.digest);
        assert_eq!(replaced.tasks.total(), 2);
    }
}
