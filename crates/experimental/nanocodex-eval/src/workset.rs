//! Durable pre-materialized evaluation tasks without scheduling policy.
//!
//! Every desired task/treatment/repetition is one SQLite row. Coordinate state
//! records scheduling and verifier outcome; an append-only attempt table keeps
//! infrastructure failures and interruptions without consuming a coordinate.
//! Ledger mutations are serialized by a process-safe writer lock. Claiming is a
//! short `BEGIN IMMEDIATE` compare-and-set transaction; execution never holds a
//! SQLite transaction open.

use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt as _;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const OBSERVER_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// One immutable task package referenced by pre-materialized rows.
#[derive(Clone, Debug)]
pub struct WorksetTask {
    /// Benchmark-visible task selector.
    pub selector: String,
    /// Loaded task name.
    pub name: String,
    /// Canonical task root.
    pub root: PathBuf,
    /// Task package content digest.
    pub digest: String,
}

/// One treatment whose repetitions become independent task rows.
#[derive(Clone, Debug)]
pub struct WorksetFamily {
    /// Stable identity of all semantic knobs except repetition.
    pub key: String,
    /// Task selector referenced by this family.
    pub task_selector: String,
    /// Harness selected for this family.
    pub harness: String,
    /// Model selected for this family.
    pub model: String,
    /// Reasoning effort selected for this family.
    pub thinking: String,
    /// Whether this family exposes model-facing web search.
    pub web_search: bool,
    /// Number of desired pre-materialized repetitions.
    pub trials: u16,
}

/// Durable SQLite benchmark ledger.
#[derive(Clone, Debug)]
pub struct Workset {
    path: PathBuf,
    claim_directory: PathBuf,
    id: i64,
    profile: String,
    digest: String,
}

/// Persistent read-only view of one existing benchmark.
pub(crate) struct WorksetObserver {
    connection: Connection,
    claim_directory: PathBuf,
    id: i64,
    profile: String,
    digest: String,
    data_version: i64,
    running_ids: Vec<i64>,
    #[cfg(test)]
    snapshot_reads: usize,
}

/// Result of atomically claiming one row from an exact family.
#[derive(Debug)]
pub enum BeginTask {
    /// One pre-materialized task row was claimed.
    Run(TaskClaim),
    /// Every unclaimed row is currently running elsewhere.
    Busy(WorksetBusy),
    /// Every row in the family is terminal.
    Complete,
}

/// Fenced ownership of one pre-materialized task row.
#[derive(Debug)]
pub struct TaskClaim {
    task_id: i64,
    claim_id: String,
    _lock: File,
    pub(crate) family_key: String,
    /// Internal repetition fixed when the benchmark was created.
    pub repetition: u16,
}

/// Temporary inability to claim an exact family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorksetBusy {
    /// Stable machine-readable reason.
    pub reason: &'static str,
    /// Suggested delay before another claim attempt.
    pub retry_after_ms: u64,
}

/// Complete four-state benchmark snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorksetStatus {
    /// Benchmark name.
    pub profile: String,
    /// Immutable benchmark digest.
    pub digest: String,
    /// Aggregate task-row counts.
    pub tasks: TaskCounts,
    /// Stable names of workers that currently own running rows.
    pub workers: Vec<String>,
    /// Terminal attempt outcomes recorded during the last five minutes.
    pub recent_attempts: RecentAttemptCounts,
    /// Exact family-level status records.
    pub families: Vec<FamilyStatus>,
}

/// Recent terminal attempt outcomes used by the occupancy controller.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RecentAttemptCounts {
    /// Successful task executions.
    pub passed: i64,
    /// Completed verifier failures.
    pub failed: i64,
    /// Infrastructure failures which returned their task to the queue.
    pub infrastructure_failed: i64,
    /// Attempts released because their owner disappeared.
    pub interrupted: i64,
    /// Newest infrastructure failures and interruptions in this window.
    pub failures: Vec<RecentAttemptFailure>,
}

/// One recent retryable attempt retained for controller diagnosis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecentAttemptFailure {
    /// Worker which owned the attempt.
    pub worker: String,
    /// `infrastructure_failed` or `interrupted`.
    pub state: String,
    /// Worker or coordinator diagnostic when one was recorded.
    pub error: Option<String>,
    /// Completion time in Unix milliseconds.
    pub finished_at_ms: i64,
}

/// Counts for the only durable task states.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct TaskCounts {
    /// Rows available for an atomic claim.
    pub unclaimed: i64,
    /// Rows whose OS ownership lock is currently held.
    pub running: i64,
    /// Rows whose verifier completed with a passing result.
    pub success: i64,
    /// Rows whose verifier completed with a failing result.
    pub failed: i64,
}

impl TaskCounts {
    /// Total number of pre-materialized rows.
    #[must_use]
    pub const fn total(&self) -> i64 {
        self.unclaimed + self.running + self.success + self.failed
    }
}

/// Status of one exact task/treatment family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FamilyStatus {
    /// Stable family identity.
    pub key: String,
    /// Benchmark-visible task selector.
    pub task: String,
    /// Harness selected for this family.
    pub harness: String,
    /// Model selected for this family.
    pub model: String,
    /// Reasoning effort selected for this family.
    pub thinking: String,
    /// Whether this family exposes model-facing web search.
    pub web_search: bool,
    /// Desired row count.
    pub desired: i64,
    /// Unclaimed row count.
    pub unclaimed: i64,
    /// Running row count.
    pub running: i64,
    /// Successful row count.
    pub success: i64,
    /// Failed row count.
    pub failed: i64,
}

/// Durable ledger failure.
#[derive(Debug, thiserror::Error)]
pub enum WorksetError {
    /// Ledger or ownership-lock I/O failed.
    #[error("evaluation workset I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite operation failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// The selected family is not part of the initialized benchmark.
    #[error("task family `{0}` is not part of this benchmark revision")]
    UnknownFamily(String),
    /// A family references a missing task definition.
    #[error("task family `{family}` references unknown task `{task}")]
    UnknownTask {
        /// Invalid family.
        family: String,
        /// Missing task selector.
        task: String,
    },
    /// An initialized benchmark disagrees with its immutable definition.
    #[error("benchmark revision `{0}` conflicts with its initialized SQLite workset")]
    DefinitionConflict(String),
    /// A stale worker attempted to finish a row it no longer owns.
    #[error("stale task claim was fenced before it could commit")]
    StaleClaim,
    /// A numeric value could not be represented by the durable schema.
    #[error("durable workset value is out of range: {0}")]
    OutOfRange(&'static str),
    /// No initialized benchmark matched the requested name.
    #[error("no initialized benchmark `{0}` exists in this SQLite ledger")]
    UnknownProfile(String),
}

impl Workset {
    /// Opens the newest generation of a named benchmark without materializing work.
    pub fn open(path: impl Into<PathBuf>, profile: &str) -> Result<Self, WorksetError> {
        let path = path.into();
        let claim_directory = claim_directory(&path);
        fs::create_dir_all(&claim_directory)?;
        let _writer = lock_workset_writer(&claim_directory)?;
        let mut connection = open_connection(&path)?;
        initialize_schema(&mut connection)?;
        let retained: Option<(i64, String)> = connection
            .query_row(
                "SELECT id, digest FROM worksets WHERE profile = ?1 \
                 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
                [profile],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((id, digest)) = retained else {
            return Err(WorksetError::UnknownProfile(profile.to_owned()));
        };
        Ok(Self {
            path,
            claim_directory,
            id,
            profile: profile.to_owned(),
            digest,
        })
    }

    /// Creates a new empty generation of a named benchmark.
    pub fn create(path: impl Into<PathBuf>, profile: &str) -> Result<Self, WorksetError> {
        let path = path.into();
        let claim_directory = claim_directory(&path);
        fs::create_dir_all(&claim_directory)?;
        let _writer = lock_workset_writer(&claim_directory)?;
        let mut connection = open_connection(&path)?;
        initialize_schema(&mut connection)?;
        let digest = Uuid::now_v7().simple().to_string();
        connection.execute(
            "INSERT INTO worksets(profile, digest, created_at_ms) VALUES (?1, ?2, ?3)",
            params![profile, digest, now_ms()?],
        )?;
        Ok(Self {
            path,
            claim_directory,
            id: connection.last_insert_rowid(),
            profile: profile.to_owned(),
            digest,
        })
    }

    /// Idempotently appends definitions and pre-materializes missing task rows.
    pub fn append(
        &self,
        tasks: &[WorksetTask],
        families: &[WorksetFamily],
    ) -> Result<(), WorksetError> {
        let _writer = lock_workset_writer(&self.claim_directory)?;
        let mut connection = open_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        append_definition(&transaction, self.id, &self.digest, tasks, families)?;
        transaction.commit()?;
        Ok(())
    }

    /// Stable identifier of this benchmark generation.
    pub(crate) fn generation(&self) -> &str {
        &self.digest
    }

    /// Loads one retained task and all of its exact treatment families.
    pub(crate) fn selected_definition(
        &self,
        selector: &str,
    ) -> Result<Option<(WorksetTask, Vec<WorksetFamily>)>, WorksetError> {
        let connection = open_connection(&self.path)?;
        let task = connection
            .query_row(
                "SELECT id, name, root, digest FROM task_definitions \
                 WHERE workset_id = ?1 AND selector = ?2",
                params![self.id, selector],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        WorksetTask {
                            selector: selector.to_owned(),
                            name: row.get(1)?,
                            root: PathBuf::from(row.get::<_, String>(2)?),
                            digest: row.get(3)?,
                        },
                    ))
                },
            )
            .optional()?;
        let Some((definition_id, task)) = task else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT family_key, harness, model, thinking, web_search, COUNT(*) FROM eval_tasks \
             WHERE workset_id = ?1 AND definition_id = ?2 \
             GROUP BY family_key, harness, model, thinking, web_search ORDER BY family_key",
        )?;
        let families = statement
            .query_map(params![self.id, definition_id], |row| {
                let trials: i64 = row.get(5)?;
                Ok(WorksetFamily {
                    key: row.get(0)?,
                    task_selector: selector.to_owned(),
                    harness: row.get(1)?,
                    model: row.get(2)?,
                    thinking: row.get(3)?,
                    web_search: row.get(4)?,
                    trials: u16::try_from(trials)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, trials))?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some((task, families)))
    }

    /// Loads the retained task and treatment selected by a family key.
    pub(crate) fn family_definition(
        &self,
        family_key: &str,
    ) -> Result<Option<(WorksetTask, WorksetFamily)>, WorksetError> {
        let connection = open_connection(&self.path)?;
        connection
            .query_row(
                "SELECT d.selector, d.name, d.root, d.digest, \
                        e.harness, e.model, e.thinking, e.web_search, COUNT(*) \
                 FROM eval_tasks e JOIN task_definitions d ON d.id = e.definition_id \
                 WHERE e.workset_id = ?1 AND e.family_key = ?2 \
                 GROUP BY d.id, e.family_key, e.harness, e.model, e.thinking, e.web_search",
                params![self.id, family_key],
                |row| {
                    let trials: i64 = row.get(8)?;
                    let selector: String = row.get(0)?;
                    Ok((
                        WorksetTask {
                            selector: selector.clone(),
                            name: row.get(1)?,
                            root: PathBuf::from(row.get::<_, String>(2)?),
                            digest: row.get(3)?,
                        },
                        WorksetFamily {
                            key: family_key.to_owned(),
                            task_selector: selector,
                            harness: row.get(4)?,
                            model: row.get(5)?,
                            thinking: row.get(6)?,
                            web_search: row.get(7)?,
                            trials: u16::try_from(trials)
                                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, trials))?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }

    /// Atomically claims one unclaimed row from the exact caller-selected family.
    pub fn begin_for_worker(
        &self,
        family_key: &str,
        worker: &str,
    ) -> Result<BeginTask, WorksetError> {
        let _writer = lock_workset_writer(&self.claim_directory)?;
        let mut connection = open_connection(&self.path)?;
        self.reconcile_abandoned_with_connection(&mut connection)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(i64, u16)> = transaction
            .query_row(
                "SELECT id, repetition FROM eval_tasks \
                 WHERE workset_id = ?1 AND family_key = ?2 AND state = 'unclaimed' \
                 ORDER BY repetition LIMIT 1",
                params![self.id, family_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((task_id, repetition)) = row {
            let lock = open_claim_lock(&self.claim_directory, task_id)?;
            lock.try_lock_exclusive()?;
            let claim_id = Uuid::now_v7().to_string();
            let started_at_ms = now_ms()?;
            let changed = transaction.execute(
                "UPDATE eval_tasks SET state = 'running', claim_id = ?1, worker = ?2, \
                    started_at_ms = ?3, finished_at_ms = NULL, result_path = NULL, error = NULL \
                 WHERE id = ?4 AND state = 'unclaimed'",
                params![claim_id, worker, started_at_ms, task_id],
            )?;
            if changed != 1 {
                return Err(WorksetError::StaleClaim);
            }
            transaction.execute(
                "DELETE FROM coordinate_results WHERE coordinate_id = ?1",
                [task_id],
            )?;
            insert_attempt(
                &transaction,
                self.id,
                task_id,
                &claim_id,
                worker,
                started_at_ms,
            )?;
            transaction.commit()?;
            return Ok(BeginTask::Run(TaskClaim {
                task_id,
                claim_id,
                _lock: lock,
                family_key: family_key.to_owned(),
                repetition,
            }));
        }
        transaction.commit()?;
        let counts: Option<TaskCounts> = connection
            .query_row(
                "SELECT \
                    COALESCE(SUM(state = 'unclaimed'), 0), \
                    COALESCE(SUM(state = 'running'), 0), \
                    COALESCE(SUM(state = 'success'), 0), \
                    COALESCE(SUM(state = 'failed'), 0) \
                 FROM eval_tasks WHERE workset_id = ?1 AND family_key = ?2",
                params![self.id, family_key],
                counts_from_row,
            )
            .optional()?;
        let Some(counts) = counts.filter(|counts| counts.total() > 0) else {
            return Err(WorksetError::UnknownFamily(family_key.to_owned()));
        };
        if counts.running > 0 {
            Ok(BeginTask::Busy(WorksetBusy {
                reason: "tasks_running",
                retry_after_ms: 1_000,
            }))
        } else {
            Ok(BeginTask::Complete)
        }
    }

    /// Atomically claims the next unclaimed row in the benchmark.
    pub fn begin_next_for_worker(&self, worker: &str) -> Result<BeginTask, WorksetError> {
        let _writer = lock_workset_writer(&self.claim_directory)?;
        let mut connection = open_connection(&self.path)?;
        self.reconcile_abandoned_with_connection(&mut connection)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(i64, String, u16)> = transaction
            .query_row(
                "SELECT id, family_key, repetition FROM eval_tasks \
                 WHERE workset_id = ?1 AND state = 'unclaimed' \
                 ORDER BY id LIMIT 1",
                [self.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((task_id, family_key, repetition)) = row {
            let lock = open_claim_lock(&self.claim_directory, task_id)?;
            lock.try_lock_exclusive()?;
            let claim_id = Uuid::now_v7().to_string();
            let started_at_ms = now_ms()?;
            let changed = transaction.execute(
                "UPDATE eval_tasks SET state = 'running', claim_id = ?1, worker = ?2, \
                    started_at_ms = ?3, finished_at_ms = NULL, result_path = NULL, error = NULL \
                 WHERE id = ?4 AND state = 'unclaimed'",
                params![claim_id, worker, started_at_ms, task_id],
            )?;
            if changed != 1 {
                return Err(WorksetError::StaleClaim);
            }
            transaction.execute(
                "DELETE FROM coordinate_results WHERE coordinate_id = ?1",
                [task_id],
            )?;
            insert_attempt(
                &transaction,
                self.id,
                task_id,
                &claim_id,
                worker,
                started_at_ms,
            )?;
            transaction.commit()?;
            return Ok(BeginTask::Run(TaskClaim {
                task_id,
                claim_id,
                _lock: lock,
                family_key,
                repetition,
            }));
        }
        transaction.commit()?;
        let counts = connection.query_row(
            "SELECT \
                COALESCE(SUM(state = 'unclaimed'), 0), \
                COALESCE(SUM(state = 'running'), 0), \
                COALESCE(SUM(state = 'success'), 0), \
                COALESCE(SUM(state = 'failed'), 0) \
             FROM eval_tasks WHERE workset_id = ?1",
            [self.id],
            counts_from_row,
        )?;
        if counts.running > 0 {
            Ok(BeginTask::Busy(WorksetBusy {
                reason: "tasks_running",
                retry_after_ms: 1_000,
            }))
        } else {
            Ok(BeginTask::Complete)
        }
    }

    /// Records a verifier-passing execution if the claim still owns the row.
    pub fn succeed(&self, claim: &TaskClaim, result_path: &Path) -> Result<(), WorksetError> {
        self.finish(claim, "success", "passed", Some(result_path), None)
    }

    /// Records a verifier-failing execution if the claim still owns the row.
    pub fn fail(
        &self,
        claim: &TaskClaim,
        result_path: Option<&Path>,
        error: &str,
    ) -> Result<(), WorksetError> {
        self.finish(claim, "failed", "failed", result_path, Some(error))
    }

    /// Retains an infrastructure-failed attempt and makes its coordinate claimable again.
    pub fn retry(
        &self,
        claim: &TaskClaim,
        result_path: Option<&Path>,
        error: &str,
    ) -> Result<(), WorksetError> {
        self.release_attempt(claim, "infrastructure_failed", result_path, Some(error))
    }

    /// Releases an interrupted execution if the claim still owns the row.
    pub fn release(&self, claim: &TaskClaim, error: &str) -> Result<(), WorksetError> {
        self.release_attempt(claim, "interrupted", None, Some(error))
    }

    /// Reacquires ownership of rows retained as running after an owner restart.
    pub(crate) fn recover_running(&self) -> Result<Vec<(TaskClaim, String)>, WorksetError> {
        let connection = open_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT id, claim_id, worker, family_key, repetition FROM eval_tasks \
             WHERE workset_id = ?1 AND state = 'running' ORDER BY id",
        )?;
        let rows = statement
            .query_map([self.id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, u16>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        rows.into_iter()
            .map(|(task_id, claim_id, worker, family_key, repetition)| {
                let lock = open_claim_lock(&self.claim_directory, task_id)?;
                lock.try_lock_exclusive()?;
                Ok((
                    TaskClaim {
                        task_id,
                        claim_id,
                        _lock: lock,
                        family_key,
                        repetition,
                    },
                    worker,
                ))
            })
            .collect()
    }

    /// Releases every running row whose OS ownership lock is no longer held.
    pub fn reconcile_abandoned(&self) -> Result<usize, WorksetError> {
        let _writer = lock_workset_writer(&self.claim_directory)?;
        let mut connection = open_connection(&self.path)?;
        self.reconcile_abandoned_with_connection(&mut connection)
    }

    fn reconcile_abandoned_with_connection(
        &self,
        connection: &mut Connection,
    ) -> Result<usize, WorksetError> {
        let mut statement = connection.prepare(
            "SELECT id, claim_id FROM eval_tasks \
             WHERE workset_id = ?1 AND state = 'running' ORDER BY id",
        )?;
        let ids = statement
            .query_map([self.id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut changed = 0;
        for (id, claim_id) in ids {
            let lock = open_claim_lock(&self.claim_directory, id)?;
            match lock.try_lock_exclusive() {
                Ok(()) => {
                    let transaction =
                        connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let released = transaction.execute(
                        "UPDATE eval_tasks SET state = 'unclaimed', claim_id = NULL, worker = NULL, \
                            started_at_ms = NULL, finished_at_ms = NULL, result_path = NULL, error = NULL \
                         WHERE id = ?1 AND state = 'running' AND claim_id = ?2",
                        params![id, claim_id],
                    )?;
                    if released == 1 {
                        transaction.execute(
                            "UPDATE eval_attempts SET state = 'interrupted', finished_at_ms = ?1, \
                                error = 'claim owner disappeared' \
                             WHERE task_id = ?2 AND claim_id = ?3 AND state = 'running'",
                            params![now_ms()?, id, claim_id],
                        )?;
                    }
                    transaction.commit()?;
                    changed += released;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(changed)
    }

    /// Reads a complete four-state snapshot after reconciling dead owners.
    pub fn status(&self) -> Result<WorksetStatus, WorksetError> {
        self.reconcile_abandoned()?;
        let connection = open_connection(&self.path)?;
        read_status(&connection, self.id, &self.profile, &self.digest)
    }

    fn finish(
        &self,
        claim: &TaskClaim,
        coordinate_state: &str,
        attempt_state: &str,
        result_path: Option<&Path>,
        error: Option<&str>,
    ) -> Result<(), WorksetError> {
        let _writer = lock_workset_writer(&self.claim_directory)?;
        let mut connection = open_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let finished_at_ms = now_ms()?;
        let changed = transaction.execute(
            "UPDATE eval_tasks SET state = ?1, finished_at_ms = ?2, result_path = ?3, error = ?4 \
             WHERE id = ?5 AND state = 'running' AND claim_id = ?6",
            params![
                coordinate_state,
                finished_at_ms,
                result_path.map(|path| path.to_string_lossy()),
                error,
                claim.task_id,
                claim.claim_id,
            ],
        )?;
        if changed != 1 {
            return Err(WorksetError::StaleClaim);
        }
        transaction.execute(
            "DELETE FROM coordinate_results WHERE coordinate_id = ?1",
            [claim.task_id],
        )?;
        finish_attempt(
            &transaction,
            claim,
            attempt_state,
            finished_at_ms,
            result_path,
            error,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn release_attempt(
        &self,
        claim: &TaskClaim,
        attempt_state: &str,
        result_path: Option<&Path>,
        error: Option<&str>,
    ) -> Result<(), WorksetError> {
        let _writer = lock_workset_writer(&self.claim_directory)?;
        let mut connection = open_connection(&self.path)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let finished_at_ms = now_ms()?;
        let changed = transaction.execute(
            "UPDATE eval_tasks SET state = 'unclaimed', claim_id = NULL, worker = NULL, \
                started_at_ms = NULL, finished_at_ms = NULL, result_path = NULL, error = NULL \
             WHERE id = ?1 AND state = 'running' AND claim_id = ?2",
            params![claim.task_id, claim.claim_id],
        )?;
        if changed != 1 {
            return Err(WorksetError::StaleClaim);
        }
        transaction.execute(
            "DELETE FROM coordinate_results WHERE coordinate_id = ?1",
            [claim.task_id],
        )?;
        finish_attempt(
            &transaction,
            claim,
            attempt_state,
            finished_at_ms,
            result_path,
            error,
        )?;
        transaction.commit()?;
        Ok(())
    }
}

impl TaskClaim {
    pub(crate) fn id(&self) -> &str {
        &self.claim_id
    }
}

fn insert_attempt(
    transaction: &rusqlite::Transaction<'_>,
    workset_id: i64,
    task_id: i64,
    claim_id: &str,
    worker: &str,
    started_at_ms: i64,
) -> Result<(), WorksetError> {
    transaction.execute(
        "INSERT INTO eval_attempts(\
            workset_id, task_id, claim_id, worker, state, started_at_ms\
         ) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
        params![workset_id, task_id, claim_id, worker, started_at_ms],
    )?;
    Ok(())
}

fn finish_attempt(
    transaction: &rusqlite::Transaction<'_>,
    claim: &TaskClaim,
    state: &str,
    finished_at_ms: i64,
    result_path: Option<&Path>,
    error: Option<&str>,
) -> Result<(), WorksetError> {
    let changed = transaction.execute(
        "UPDATE eval_attempts SET state = ?1, finished_at_ms = ?2, result_path = ?3, error = ?4 \
         WHERE task_id = ?5 AND claim_id = ?6 AND state = 'running'",
        params![
            state,
            finished_at_ms,
            result_path.map(|path| path.to_string_lossy()),
            error,
            claim.task_id,
            claim.claim_id,
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(WorksetError::StaleClaim)
    }
}

fn append_definition(
    transaction: &rusqlite::Transaction<'_>,
    workset_id: i64,
    generation: &str,
    tasks: &[WorksetTask],
    families: &[WorksetFamily],
) -> Result<(), WorksetError> {
    for task in tasks {
        transaction.execute(
            "INSERT OR IGNORE INTO task_definitions(workset_id, selector, name, root, digest) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                workset_id,
                task.selector,
                task.name,
                task.root.to_string_lossy(),
                task.digest
            ],
        )?;
        let retained: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT name, root, digest FROM task_definitions \
                 WHERE workset_id = ?1 AND selector = ?2",
                params![workset_id, task.selector],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if retained.as_ref()
            != Some(&(
                task.name.clone(),
                task.root.to_string_lossy().into_owned(),
                task.digest.clone(),
            ))
        {
            return Err(WorksetError::DefinitionConflict(generation.to_owned()));
        }
    }
    for family in families {
        let definition_id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM task_definitions WHERE workset_id = ?1 AND selector = ?2",
                params![workset_id, family.task_selector],
                |row| row.get(0),
            )
            .optional()?;
        let Some(definition_id) = definition_id else {
            return Err(WorksetError::UnknownTask {
                family: family.key.clone(),
                task: family.task_selector.clone(),
            });
        };
        let retained_family: Option<(String, String, String, bool)> = transaction
            .query_row(
                "SELECT harness, model, thinking, web_search FROM eval_tasks \
                 WHERE workset_id = ?1 AND family_key = ?2 LIMIT 1",
                params![workset_id, family.key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if retained_family.as_ref().is_some_and(|retained| {
            retained
                != &(
                    family.harness.clone(),
                    family.model.clone(),
                    family.thinking.clone(),
                    family.web_search,
                )
        }) {
            return Err(WorksetError::DefinitionConflict(generation.to_owned()));
        }
        for repetition in 1..=family.trials {
            transaction.execute(
                "INSERT OR IGNORE INTO eval_tasks( \
                    workset_id, definition_id, family_key, harness, model, thinking, web_search, \
                    repetition, state \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'unclaimed')",
                params![
                    workset_id,
                    definition_id,
                    family.key,
                    family.harness,
                    family.model,
                    family.thinking,
                    family.web_search,
                    repetition
                ],
            )?;
        }
    }
    Ok(())
}

fn read_status(
    connection: &Connection,
    workset_id: i64,
    profile: &str,
    digest: &str,
) -> Result<WorksetStatus, WorksetError> {
    let tasks = connection.query_row(
        "SELECT \
            COALESCE(SUM(state = 'unclaimed'), 0), \
            COALESCE(SUM(state = 'running'), 0), \
            COALESCE(SUM(state = 'success'), 0), \
            COALESCE(SUM(state = 'failed'), 0) \
         FROM eval_tasks WHERE workset_id = ?1",
        [workset_id],
        counts_from_row,
    )?;
    let mut worker_statement = connection.prepare(
        "SELECT DISTINCT worker FROM eval_tasks \
         WHERE workset_id = ?1 AND state = 'running' AND worker IS NOT NULL ORDER BY worker",
    )?;
    let workers = worker_statement
        .query_map([workset_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let recent_cutoff_ms = now_ms()?.saturating_sub(5 * 60 * 1_000);
    let recent_attempts = connection.query_row(
        "SELECT \
            COALESCE(SUM(a.state = 'passed'), 0), \
            COALESCE(SUM(a.state = 'failed'), 0), \
            COALESCE(SUM(a.state = 'infrastructure_failed'), 0), \
            COALESCE(SUM(a.state = 'interrupted'), 0) \
         FROM eval_attempts a \
         WHERE a.workset_id = ?1 AND a.finished_at_ms >= ?2",
        params![workset_id, recent_cutoff_ms],
        |row| {
            Ok(RecentAttemptCounts {
                passed: row.get(0)?,
                failed: row.get(1)?,
                infrastructure_failed: row.get(2)?,
                interrupted: row.get(3)?,
                failures: Vec::new(),
            })
        },
    )?;
    let mut recent_attempts = recent_attempts;
    let mut failure_statement = connection.prepare(
        "SELECT worker, state, error, finished_at_ms FROM eval_attempts \
         WHERE workset_id = ?1 AND finished_at_ms >= ?2 \
            AND state IN ('infrastructure_failed', 'interrupted') \
         ORDER BY finished_at_ms DESC LIMIT 8",
    )?;
    recent_attempts.failures = failure_statement
        .query_map(params![workset_id, recent_cutoff_ms], |row| {
            Ok(RecentAttemptFailure {
                worker: row.get(0)?,
                state: row.get(1)?,
                error: row.get(2)?,
                finished_at_ms: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut statement = connection.prepare(
        "SELECT e.family_key, d.selector, e.harness, e.model, e.thinking, e.web_search, COUNT(*), \
            COALESCE(SUM(e.state = 'unclaimed'), 0), \
            COALESCE(SUM(e.state = 'running'), 0), \
            COALESCE(SUM(e.state = 'success'), 0), \
            COALESCE(SUM(e.state = 'failed'), 0) \
         FROM eval_tasks e JOIN task_definitions d ON d.id = e.definition_id \
         WHERE e.workset_id = ?1 \
         GROUP BY e.family_key, d.selector, e.harness, e.model, e.thinking, e.web_search \
         ORDER BY d.selector, e.family_key",
    )?;
    let families = statement
        .query_map([workset_id], |row| {
            Ok(FamilyStatus {
                key: row.get(0)?,
                task: row.get(1)?,
                harness: row.get(2)?,
                model: row.get(3)?,
                thinking: row.get(4)?,
                web_search: row.get(5)?,
                desired: row.get(6)?,
                unclaimed: row.get(7)?,
                running: row.get(8)?,
                success: row.get(9)?,
                failed: row.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorksetStatus {
        profile: profile.to_owned(),
        digest: digest.to_owned(),
        tasks,
        workers,
        recent_attempts,
        families,
    })
}

fn counts_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskCounts> {
    Ok(TaskCounts {
        unclaimed: row.get(0)?,
        running: row.get(1)?,
        success: row.get(2)?,
        failed: row.get(3)?,
    })
}

impl WorksetObserver {
    pub(crate) fn open(path: &Path, profile: &str) -> Result<Self, WorksetError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(OBSERVER_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "query_only", "ON")?;
        let schema_version: u32 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version != SCHEMA_VERSION {
            return Err(WorksetError::DefinitionConflict(format!(
                "schema {schema_version}; expected {SCHEMA_VERSION}"
            )));
        }
        let mut statement = connection.prepare(
            "SELECT id, profile, digest FROM worksets WHERE profile = ?1 \
             ORDER BY created_at_ms DESC, id DESC LIMIT 1",
        )?;
        let mut matches = statement
            .query_map([profile], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<(i64, String, String)>, _>>()?;
        drop(statement);
        if matches.is_empty() {
            return Err(WorksetError::UnknownProfile(profile.to_owned()));
        }
        let (id, profile, digest) = matches
            .pop()
            .ok_or_else(|| WorksetError::UnknownProfile(profile.to_owned()))?;
        let data_version = sqlite_data_version(&connection)?;
        Ok(Self {
            connection,
            claim_directory: claim_directory(path),
            id,
            profile,
            digest,
            data_version,
            running_ids: Vec::new(),
            #[cfg(test)]
            snapshot_reads: 0,
        })
    }

    pub(crate) fn snapshot(&mut self) -> Result<WorksetStatus, WorksetError> {
        self.snapshot_inner()
    }

    pub(crate) fn refresh(&mut self) -> Result<Option<WorksetStatus>, WorksetError> {
        let data_version = sqlite_data_version(&self.connection)?;
        if data_version == self.data_version && !self.has_abandoned_owner()? {
            return Ok(None);
        }
        self.snapshot_inner().map(Some)
    }

    fn snapshot_inner(&mut self) -> Result<WorksetStatus, WorksetError> {
        self.data_version = sqlite_data_version(&self.connection)?;
        self.running_ids = running_ids(&self.connection, self.id)?;
        let mut status = read_status(&self.connection, self.id, &self.profile, &self.digest)?;
        let abandoned = self.abandoned_ids()?;
        if !abandoned.is_empty() {
            let count = i64::try_from(abandoned.len())
                .map_err(|_| WorksetError::OutOfRange("abandoned task count"))?;
            status.tasks.running -= count;
            status.tasks.unclaimed += count;
            for family in &mut status.families {
                let family_abandoned =
                    abandoned_in_family(&self.connection, &abandoned, &family.key)?;
                family.running -= family_abandoned;
                family.unclaimed += family_abandoned;
            }
        }
        #[cfg(test)]
        {
            self.snapshot_reads += 1;
        }
        Ok(status)
    }

    fn has_abandoned_owner(&self) -> Result<bool, WorksetError> {
        Ok(!self.abandoned_ids()?.is_empty())
    }

    fn abandoned_ids(&self) -> Result<Vec<i64>, WorksetError> {
        let mut abandoned = Vec::new();
        for &id in &self.running_ids {
            let lock = open_claim_lock(&self.claim_directory, id)?;
            match lock.try_lock_exclusive() {
                Ok(()) => abandoned.push(id),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(abandoned)
    }
}

fn abandoned_in_family(
    connection: &Connection,
    ids: &[i64],
    family: &str,
) -> Result<i64, WorksetError> {
    let mut count = 0;
    for id in ids {
        count += connection.query_row(
            "SELECT COUNT(*) FROM eval_tasks WHERE id = ?1 AND family_key = ?2",
            params![id, family],
            |row| row.get::<_, i64>(0),
        )?;
    }
    Ok(count)
}

fn running_ids(connection: &Connection, workset_id: i64) -> Result<Vec<i64>, WorksetError> {
    let mut statement = connection
        .prepare("SELECT id FROM eval_tasks WHERE workset_id = ?1 AND state = 'running'")?;
    Ok(statement
        .query_map([workset_id], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn claim_directory(path: &Path) -> PathBuf {
    path.with_extension("claims")
}

fn open_claim_lock(directory: &Path, id: i64) -> Result<File, WorksetError> {
    fs::create_dir_all(directory)?;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!("{id}.lock")))?)
}

fn lock_workset_writer(directory: &Path) -> Result<File, WorksetError> {
    fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("writer.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}

fn sqlite_data_version(connection: &Connection) -> Result<i64, WorksetError> {
    Ok(connection.pragma_query_value(None, "data_version", |row| row.get(0))?)
}

fn open_connection(path: &Path) -> Result<Connection, WorksetError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

fn initialize_schema(connection: &mut Connection) -> Result<(), WorksetError> {
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != 0 && version != SCHEMA_VERSION {
        return Err(WorksetError::DefinitionConflict(format!(
            "schema {version}; expected {SCHEMA_VERSION}"
        )));
    }
    if version == 0 {
        connection.pragma_update(None, "journal_mode", "WAL")?;
    }
    create_schema(connection)?;
    if version == 0 {
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

fn create_schema(connection: &Connection) -> Result<(), WorksetError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS worksets(
            id INTEGER PRIMARY KEY,
            profile TEXT NOT NULL,
            digest TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            UNIQUE(profile, digest)
         );
         CREATE INDEX IF NOT EXISTS worksets_digest
            ON worksets(digest);
         CREATE TABLE IF NOT EXISTS task_definitions(
            id INTEGER PRIMARY KEY,
            workset_id INTEGER NOT NULL REFERENCES worksets(id),
            selector TEXT NOT NULL,
            name TEXT NOT NULL,
            root TEXT NOT NULL,
            digest TEXT NOT NULL,
            UNIQUE(workset_id, selector)
         );
         CREATE TABLE IF NOT EXISTS eval_tasks(
            id INTEGER PRIMARY KEY,
            workset_id INTEGER NOT NULL REFERENCES worksets(id),
            definition_id INTEGER NOT NULL REFERENCES task_definitions(id),
            family_key TEXT NOT NULL,
            harness TEXT NOT NULL DEFAULT '',
            model TEXT NOT NULL DEFAULT '',
            thinking TEXT NOT NULL DEFAULT '',
            web_search INTEGER NOT NULL DEFAULT 0,
            repetition INTEGER NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('unclaimed','running','success','failed')),
            claim_id TEXT,
            worker TEXT,
            started_at_ms INTEGER,
            finished_at_ms INTEGER,
            result_path TEXT,
            error TEXT,
            UNIQUE(workset_id, family_key, repetition),
            CHECK(
                (state = 'unclaimed' AND claim_id IS NULL AND started_at_ms IS NULL AND finished_at_ms IS NULL) OR
                (state = 'running' AND claim_id IS NOT NULL AND started_at_ms IS NOT NULL AND finished_at_ms IS NULL) OR
                (state IN ('success','failed') AND claim_id IS NOT NULL AND started_at_ms IS NOT NULL AND finished_at_ms IS NOT NULL)
            )
         );
         CREATE INDEX IF NOT EXISTS eval_tasks_claimable
            ON eval_tasks(workset_id, family_key, state, repetition);
         CREATE INDEX IF NOT EXISTS eval_tasks_next
            ON eval_tasks(workset_id, state, id);
         CREATE INDEX IF NOT EXISTS eval_tasks_definition
            ON eval_tasks(workset_id, definition_id);
         CREATE INDEX IF NOT EXISTS eval_tasks_result_path
            ON eval_tasks(result_path) WHERE result_path IS NOT NULL;
         CREATE TABLE IF NOT EXISTS coordinate_results(
            coordinate_id INTEGER PRIMARY KEY REFERENCES eval_tasks(id),
            result_path TEXT,
            status TEXT,
            outcome TEXT,
            input_tokens INTEGER,
            cached_input_tokens INTEGER,
            output_tokens INTEGER,
            reasoning_output_tokens INTEGER,
            total_tokens INTEGER,
            cost_usd REAL,
            agent_duration_ms INTEGER
         );
         CREATE TABLE IF NOT EXISTS eval_attempts(
            id INTEGER PRIMARY KEY,
            workset_id INTEGER NOT NULL REFERENCES worksets(id),
            task_id INTEGER NOT NULL REFERENCES eval_tasks(id),
            claim_id TEXT NOT NULL UNIQUE,
            worker TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN (
                'running','passed','failed','infrastructure_failed','interrupted'
            )),
            started_at_ms INTEGER NOT NULL,
            finished_at_ms INTEGER,
            result_path TEXT,
            error TEXT,
            CHECK(
                (state = 'running' AND finished_at_ms IS NULL) OR
                (state != 'running' AND finished_at_ms IS NOT NULL)
            )
         );
         CREATE INDEX IF NOT EXISTS eval_attempts_task
            ON eval_attempts(task_id, started_at_ms);
         CREATE INDEX IF NOT EXISTS eval_attempts_worker
            ON eval_attempts(worker, state);
         CREATE INDEX IF NOT EXISTS eval_attempts_recent
            ON eval_attempts(workset_id, finished_at_ms);",
    )?;
    Ok(())
}

fn now_ms() -> Result<i64, WorksetError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WorksetError::OutOfRange("system time"))?
        .as_millis();
    i64::try_from(millis).map_err(|_| WorksetError::OutOfRange("system time"))
}

#[cfg(test)]
mod tests {
    use std::{process::Command, sync::Arc, thread, time::Instant};

    use super::*;

    fn definition(root: &Path, trials: u16) -> (Vec<WorksetTask>, Vec<WorksetFamily>) {
        (
            vec![WorksetTask {
                selector: "terminal/fix-git".to_owned(),
                name: "fix-git".to_owned(),
                root: root.join("fix-git"),
                digest: "task-digest".to_owned(),
            }],
            vec![WorksetFamily {
                key: "terminal/fix-git|harness|high".to_owned(),
                task_selector: "terminal/fix-git".to_owned(),
                harness: "codex".to_owned(),
                model: "luna".to_owned(),
                thinking: "high".to_owned(),
                web_search: false,
                trials,
            }],
        )
    }

    fn workset(directory: &Path, trials: u16) -> Workset {
        let workset = Workset::create(directory.join("state.sqlite3"), "release").unwrap();
        let (tasks, families) = definition(directory, trials);
        workset.append(&tasks, &families).unwrap();
        workset
    }

    #[test]
    fn append_pre_materializes_every_task_row() {
        let directory = tempfile::tempdir().unwrap();
        let workset = workset(directory.path(), 3);
        let status = workset.status().unwrap();
        assert_eq!(status.tasks.unclaimed, 3);
        assert_eq!(status.tasks.total(), 3);
        assert_eq!(status.families[0].desired, 3);
    }

    #[test]
    fn append_only_materializes_missing_rows_and_new_generation_is_independent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let first = Workset::create(&path, "release").unwrap();
        let (tasks, one) = definition(directory.path(), 1);
        first.append(&tasks, &one).unwrap();
        let (_, three) = definition(directory.path(), 3);
        first.append(&tasks, &three).unwrap();
        assert_eq!(first.status().unwrap().tasks.unclaimed, 3);

        let second = Workset::create(&path, "release").unwrap();
        let (_, two) = definition(directory.path(), 2);
        second.append(&tasks, &two).unwrap();
        assert_ne!(first.generation(), second.generation());
        assert_eq!(
            Workset::open(&path, "release")
                .unwrap()
                .status()
                .unwrap()
                .tasks
                .total(),
            2
        );
        assert_eq!(first.status().unwrap().tasks.total(), 3);
    }

    #[test]
    fn concurrent_claimers_never_receive_the_same_row() {
        let directory = tempfile::tempdir().unwrap();
        const WORKERS: usize = 64;
        const TASKS: usize = 1_024;
        let workset = Arc::new(workset(directory.path(), u16::try_from(TASKS).unwrap()));
        let mut threads = Vec::new();
        for worker in 0..WORKERS {
            let workset = workset.clone();
            threads.push(thread::spawn(move || {
                let mut repetitions = Vec::new();
                while let BeginTask::Run(claim) = workset
                    .begin_next_for_worker(&format!("worker-{worker}"))
                    .unwrap()
                {
                    repetitions.push(claim.repetition);
                    workset.succeed(&claim, Path::new("evidence")).unwrap();
                }
                repetitions
            }));
        }
        let mut repetitions = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        repetitions.sort_unstable();
        repetitions.dedup();
        assert_eq!(repetitions.len(), TASKS);
        let status = workset.status().unwrap();
        assert_eq!(status.tasks.success, i64::try_from(TASKS).unwrap());
        assert_eq!(status.tasks.running, 0);
    }

    #[test]
    fn dropping_owner_lock_makes_running_row_reclaimable() {
        let directory = tempfile::tempdir().unwrap();
        let workset = workset(directory.path(), 1);
        let BeginTask::Run(claim) = workset
            .begin_for_worker("terminal/fix-git|harness|high", "worker")
            .unwrap()
        else {
            panic!("row should be claimable");
        };
        drop(claim);
        let status = workset.status().unwrap();
        assert_eq!(status.tasks.running, 0);
        assert_eq!(status.tasks.failed, 0);
        assert_eq!(status.tasks.unclaimed, 1);
        assert!(matches!(
            workset
                .begin_for_worker("terminal/fix-git|harness|high", "replacement")
                .unwrap(),
            BeginTask::Run(_)
        ));
    }

    #[test]
    fn owner_process_death_releases_the_lock_and_reopens_the_row() {
        const CHILD_STATE: &str = "NANOCODEX_WORKSET_CRASH_CHILD";
        if let Some(directory) = std::env::var_os(CHILD_STATE) {
            let directory = PathBuf::from(directory);
            let workset = Workset::open(directory.join("state.sqlite3"), "release").unwrap();
            let _claim = match workset.begin_next_for_worker("crash-child").unwrap() {
                BeginTask::Run(claim) => claim,
                other => panic!("child expected a running claim, got {other:?}"),
            };
            fs::write(directory.join("child-ready"), b"ready").unwrap();
            thread::sleep(Duration::from_secs(60));
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let workset = workset(directory.path(), 1);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workset::tests::owner_process_death_releases_the_lock_and_reopens_the_row",
                "--nocapture",
            ])
            .env(CHILD_STATE, directory.path())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.path().join("child-ready").is_file() {
            assert!(
                Instant::now() < deadline,
                "crash child never acquired its row"
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "crash child exited early"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(workset.status().unwrap().tasks.running, 1);
        child.kill().unwrap();
        child.wait().unwrap();
        let status = workset.status().unwrap();
        assert_eq!(status.tasks.running, 0);
        assert_eq!(status.tasks.failed, 0);
        assert_eq!(status.tasks.unclaimed, 1);
        assert!(matches!(
            workset.begin_next_for_worker("replacement").unwrap(),
            BeginTask::Run(_)
        ));
    }

    #[test]
    fn stale_claim_cannot_overwrite_a_terminal_row() {
        let directory = tempfile::tempdir().unwrap();
        let workset = workset(directory.path(), 1);
        let BeginTask::Run(claim) = workset
            .begin_for_worker("terminal/fix-git|harness|high", "worker")
            .unwrap()
        else {
            panic!("row should be claimable");
        };
        workset.fail(&claim, None, "boom").unwrap();
        assert!(matches!(
            workset.succeed(&claim, Path::new("late")),
            Err(WorksetError::StaleClaim)
        ));
    }

    #[test]
    fn infrastructure_failure_is_retained_as_an_attempt_and_requeues_the_row() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let workset = Workset::create(&path, "release").unwrap();
        let (tasks, families) = definition(directory.path(), 1);
        workset.append(&tasks, &families).unwrap();
        let BeginTask::Run(claim) = workset
            .begin_for_worker("terminal/fix-git|harness|high", "worker-one")
            .unwrap()
        else {
            panic!("row should be claimable");
        };
        let evidence = directory.path().join("attempt-one/events.jsonl");
        workset
            .retry(&claim, Some(&evidence), "provider returned 429")
            .unwrap();
        drop(claim);

        let status = workset.status().unwrap();
        assert_eq!(status.tasks.unclaimed, 1);
        assert_eq!(status.tasks.running, 0);
        assert_eq!(status.tasks.failed, 0);
        let connection = open_connection(&path).unwrap();
        let attempt: (String, String, String, String) = connection
            .query_row(
                "SELECT worker, state, result_path, error FROM eval_attempts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            attempt,
            (
                "worker-one".to_owned(),
                "infrastructure_failed".to_owned(),
                evidence.to_string_lossy().into_owned(),
                "provider returned 429".to_owned(),
            )
        );

        let BeginTask::Run(retry) = workset
            .begin_for_worker("terminal/fix-git|harness|high", "worker-two")
            .unwrap()
        else {
            panic!("infrastructure-failed row should be claimable again");
        };
        workset
            .fail(
                &retry,
                Some(Path::new("scored/events.jsonl")),
                "verifier failed",
            )
            .unwrap();
        let status = workset.status().unwrap();
        assert_eq!(status.tasks.unclaimed, 0);
        assert_eq!(status.tasks.failed, 1);
        let attempts: Vec<String> = connection
            .prepare("SELECT state FROM eval_attempts ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(attempts, ["infrastructure_failed", "failed"]);
    }

    #[test]
    fn observer_is_read_only_and_notices_owner_death_without_sqlite_commit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let workset = Workset::create(&path, "release").unwrap();
        let (tasks, families) = definition(directory.path(), 1);
        workset.append(&tasks, &families).unwrap();
        let BeginTask::Run(claim) = workset
            .begin_for_worker("terminal/fix-git|harness|high", "worker")
            .unwrap()
        else {
            panic!("row should be claimable");
        };
        let mut observer = WorksetObserver::open(&path, "release").unwrap();
        assert_eq!(observer.snapshot().unwrap().tasks.running, 1);
        drop(claim);
        let refreshed = observer.refresh().unwrap().unwrap();
        assert_eq!(refreshed.tasks.running, 0);
        assert_eq!(refreshed.tasks.unclaimed, 1);
        assert_eq!(refreshed.tasks.failed, 0);
        assert!(
            observer
                .connection
                .execute("DELETE FROM eval_tasks", [])
                .is_err()
        );
    }

    #[test]
    fn observer_does_not_create_a_missing_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        assert!(WorksetObserver::open(&path, "release").is_err());
        assert!(!path.exists());
    }

    #[test]
    fn unknown_family_never_expands_the_closed_benchmark() {
        let directory = tempfile::tempdir().unwrap();
        let workset = workset(directory.path(), 1);
        assert!(matches!(
            workset.begin_for_worker("missing", "worker"),
            Err(WorksetError::UnknownFamily(family)) if family == "missing"
        ));
        assert_eq!(workset.status().unwrap().tasks.unclaimed, 1);
    }

    #[test]
    fn future_schema_is_rejected_without_rewriting_its_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        assert!(matches!(
            Workset::open(&path, "release"),
            Err(WorksetError::DefinitionConflict(message)) if message.contains("schema 99")
        ));
    }

    #[test]
    fn path_is_the_sqlite_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let workset = workset(directory.path(), 1);
        assert_eq!(workset.path(), directory.path().join("state.sqlite3"));
    }
}
