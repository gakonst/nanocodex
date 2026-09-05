use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::DateTime;
use nanocodex_oai_api::{
    Model,
    pricing::{ServiceTier, estimate_for_model},
    responses::{InputTokenDetails, Usage},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const LEDGER_FILE: &str = "state.sqlite3";
const API_SCHEMA_VERSION: u32 = 4;
const MAX_EVENT_LINE_BYTES: usize = 8 * 1024 * 1024;
const OUTCOME_TAIL_BYTES: u64 = 512 * 1024;

#[derive(Clone)]
pub(crate) struct EvalApi {
    ledger: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvalSummary {
    total: u64,
    unclaimed: u64,
    running: u64,
    success: u64,
    failed: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvalOverview {
    schema_version: u32,
    observed_at_ms: i64,
    summary: EvalSummary,
    worksets: Vec<WorksetOverview>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorksetOverview {
    id: String,
    profile: String,
    digest: String,
    created_at_ms: i64,
    task_count: u64,
    summary: EvalSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorksetDetail {
    schema_version: u32,
    observed_at_ms: i64,
    workset: WorksetOverview,
    tasks: Vec<TaskOverview>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorksetResults {
    schema_version: u32,
    observed_at_ms: i64,
    workset_id: String,
    points: Vec<ResultPoint>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorksetAnalytics {
    schema_version: u32,
    observed_at_ms: i64,
    workset_id: String,
    task_count: u64,
    points: Vec<AnalyticsPoint>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalyticsPoint {
    harness: String,
    model: String,
    thinking: String,
    passed: u64,
    completed: u64,
    median_output_tokens: Option<f64>,
    output_samples: usize,
    median_duration_ms: Option<f64>,
    duration_samples: usize,
    median_cost_usd: Option<f64>,
    cost_samples: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResultPoint {
    id: String,
    task_id: String,
    task_name: String,
    task_label: String,
    state: String,
    harness: String,
    model: String,
    thinking: String,
    repetition: u16,
    status: Option<String>,
    outcome: Option<String>,
    duration_ms: Option<i64>,
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    cost_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskOverview {
    id: String,
    name: String,
    label: String,
    digest: String,
    treatment_count: u64,
    summary: EvalSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskDetail {
    schema_version: u32,
    observed_at_ms: i64,
    workset_id: String,
    task: TaskMatrix,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskOutcomesPage {
    schema_version: u32,
    observed_at_ms: i64,
    workset_id: String,
    task_id: String,
    total: usize,
    next_cursor: Option<usize>,
    outcomes: Vec<CoordinateOutcome>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoordinateOutcome {
    id: String,
    status: Option<String>,
    outcome: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskMatrix {
    id: String,
    name: String,
    label: String,
    digest: String,
    treatments: Vec<TreatmentDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TreatmentDetail {
    id: String,
    label: String,
    harness: String,
    model: String,
    thinking: String,
    cells: Vec<CoordinateDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoordinateDetail {
    id: String,
    repetition: u16,
    state: &'static str,
    status: Option<String>,
    outcome: Option<String>,
    updated_at_ms: Option<i64>,
    duration_ms: Option<i64>,
    message: Option<String>,
    detail_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaseEvidence {
    schema_version: u32,
    task_name: Option<String>,
    prompt: Option<String>,
    status: Option<String>,
    outcome: Option<String>,
    environment: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    final_message: Option<String>,
    tool_calls: Option<u64>,
    usage: Option<Value>,
    cost_usd: Option<f64>,
    verifier: Option<Value>,
    exception: Option<Value>,
    timing: Option<Value>,
    verifier_stdout: Option<String>,
    verifier_stderr: Option<String>,
}

#[derive(Debug)]
struct WorksetRow {
    id: i64,
    profile: String,
    digest: String,
    created_at_ms: i64,
}

#[derive(Debug)]
struct TaskRow {
    id: i64,
    name: String,
    digest: String,
}

#[derive(Debug)]
struct CoordinateRow {
    id: i64,
    family_key: String,
    harness: String,
    model: String,
    thinking: String,
    repetition: u16,
    state: String,
    result_path: Option<PathBuf>,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    error: Option<String>,
    status: Option<String>,
    outcome: Option<String>,
    agent_duration_ms: Option<i64>,
}

#[derive(Debug)]
struct AnalyticsGroup {
    harness: String,
    model: String,
    thinking: String,
    passed: u64,
    completed: u64,
    output_tokens: Vec<i64>,
    durations_ms: Vec<i64>,
    costs_usd: Vec<f64>,
}

impl EvalApi {
    pub(crate) fn new(state_directory: &Path) -> Self {
        Self {
            ledger: state_directory.join(LEDGER_FILE),
        }
    }

    pub(crate) fn overview(&self) -> Result<EvalOverview, String> {
        let connection = self.connection()?;
        let now = now_ms()?;
        let mut total = EvalSummary::default();
        let mut statement = connection
            .prepare(
                "SELECT w.profile, w.digest, w.created_at_ms, \
                        COALESCE(d.task_count, 0), \
                        COALESCE(s.total, 0), COALESCE(s.unclaimed, 0), \
                        COALESCE(s.running, 0), COALESCE(s.success, 0), \
                        COALESCE(s.failed, 0) \
                 FROM worksets w \
                 LEFT JOIN ( \
                    SELECT workset_id, COUNT(*) AS task_count \
                    FROM task_definitions GROUP BY workset_id \
                 ) d ON d.workset_id = w.id \
                 LEFT JOIN ( \
                    SELECT workset_id, COUNT(*) AS total, \
                           SUM(state = 'unclaimed') AS unclaimed, \
                           SUM(state = 'running') AS running, \
                           SUM(state = 'success') AS success, \
                           SUM(state = 'failed') AS failed \
                    FROM eval_tasks GROUP BY workset_id \
                 ) s ON s.workset_id = w.id \
                 ORDER BY w.created_at_ms DESC, w.id DESC",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                let summary = summary_from_row(row, 4)?;
                Ok((
                    WorksetOverview {
                        id: row.get(1)?,
                        profile: row.get(0)?,
                        digest: row.get(1)?,
                        created_at_ms: row.get(2)?,
                        task_count: nonnegative_count(row, 3)?,
                        summary: summary.clone(),
                    },
                    summary,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut overview = Vec::new();
        for row in rows {
            let (workset, summary) = row.map_err(|error| error.to_string())?;
            add_summary(&mut total, &summary);
            overview.push(workset);
        }
        Ok(EvalOverview {
            schema_version: API_SCHEMA_VERSION,
            observed_at_ms: now,
            summary: total,
            worksets: overview,
        })
    }

    pub(crate) fn workset(&self, digest: &str) -> Result<Option<WorksetDetail>, String> {
        let connection = self.connection()?;
        let Some(workset) = find_workset(&connection, digest)? else {
            return Ok(None);
        };
        let now = now_ms()?;
        let tasks = read_task_overviews(&connection, &workset)?;
        let mut summary = EvalSummary::default();
        for task in &tasks {
            add_summary(&mut summary, &task.summary);
        }
        let workset_overview = WorksetOverview {
            id: workset.digest.clone(),
            profile: workset.profile,
            digest: workset.digest.clone(),
            created_at_ms: workset.created_at_ms,
            task_count: u64::try_from(tasks.len()).map_err(|error| error.to_string())?,
            summary,
        };
        Ok(Some(WorksetDetail {
            schema_version: API_SCHEMA_VERSION,
            observed_at_ms: now,
            workset: workset_overview,
            tasks,
        }))
    }

    pub(crate) fn workset_analytics(
        &self,
        digest: &str,
    ) -> Result<Option<WorksetAnalytics>, String> {
        let connection = self.connection()?;
        let Some(workset) = find_workset(&connection, digest)? else {
            return Ok(None);
        };
        let task_count = connection
            .query_row(
                "SELECT COUNT(*) FROM task_definitions WHERE workset_id = ?1",
                [workset.id],
                |row| nonnegative_count(row, 0),
            )
            .map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT e.harness, e.model, e.thinking, e.state, \
                        r.agent_duration_ms, r.status, r.outcome, \
                        r.input_tokens, r.cached_input_tokens, r.output_tokens, r.cost_usd \
                 FROM eval_tasks e \
                 LEFT JOIN coordinate_results r ON r.coordinate_id = e.id \
                 WHERE e.workset_id = ?1 AND e.state IN ('success', 'failed') \
                   AND e.result_path IS NOT NULL",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([workset.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<f64>>(10)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut groups = HashMap::<String, AnalyticsGroup>::new();
        for row in rows {
            let (
                harness,
                model,
                thinking,
                state,
                agent_duration_ms,
                status,
                outcome,
                input,
                cached_input,
                output,
                cost,
            ) = row.map_err(|error| error.to_string())?;
            if outcome.as_deref() == Some("infrastructure_error") {
                continue;
            }
            let cost = cost.or_else(|| estimated_cost_usd(&model, input, cached_input, output));
            let key = format!("{harness}\0{model}\0{thinking}");
            let group = groups.entry(key).or_insert_with(|| AnalyticsGroup {
                harness,
                model,
                thinking,
                passed: 0,
                completed: 0,
                output_tokens: Vec::new(),
                durations_ms: Vec::new(),
                costs_usd: Vec::new(),
            });
            group.completed += 1;
            if retained_result_passed(&state, status.as_deref(), outcome.as_deref()) {
                group.passed += 1;
            }
            if let Some(output) = output.filter(|value| *value >= 0) {
                group.output_tokens.push(output);
            }
            if let Some(duration) = agent_duration_ms.filter(|value| *value >= 0) {
                group.durations_ms.push(duration);
            }
            if let Some(cost) = cost.filter(|value| value.is_finite() && *value >= 0.0) {
                group.costs_usd.push(cost);
            }
        }
        let mut points = groups
            .into_values()
            .map(|mut group| AnalyticsPoint {
                harness: group.harness,
                model: group.model,
                thinking: group.thinking,
                passed: group.passed,
                completed: group.completed,
                median_output_tokens: median_i64(&mut group.output_tokens),
                output_samples: group.output_tokens.len(),
                median_duration_ms: median_i64(&mut group.durations_ms),
                duration_samples: group.durations_ms.len(),
                median_cost_usd: median_f64(&mut group.costs_usd),
                cost_samples: group.costs_usd.len(),
            })
            .collect::<Vec<_>>();
        points.sort_by(|left, right| {
            left.harness
                .cmp(&right.harness)
                .then_with(|| left.model.cmp(&right.model))
                .then_with(|| left.thinking.cmp(&right.thinking))
        });
        Ok(Some(WorksetAnalytics {
            schema_version: API_SCHEMA_VERSION,
            observed_at_ms: now_ms()?,
            workset_id: workset.digest,
            task_count,
            points,
        }))
    }

    pub(crate) fn task_results(
        &self,
        digest: &str,
        task_id: &str,
    ) -> Result<Option<WorksetResults>, String> {
        self.results(digest, task_id)
    }

    fn results(&self, digest: &str, task_id: &str) -> Result<Option<WorksetResults>, String> {
        let connection = self.connection()?;
        let Some(workset) = find_workset(&connection, digest)? else {
            return Ok(None);
        };
        let Some(task) = find_task(&connection, workset.id, digest, task_id)? else {
            return Ok(None);
        };
        let mut statement = connection
            .prepare(
                "SELECT e.id, t.selector, e.state, e.harness, e.model, e.thinking, \
                        e.repetition, r.agent_duration_ms, r.status, r.outcome, \
                        r.input_tokens, r.cached_input_tokens, \
                        r.output_tokens, r.reasoning_output_tokens, r.total_tokens, r.cost_usd \
                 FROM eval_tasks e INDEXED BY eval_tasks_definition \
                 JOIN task_definitions t ON t.id = e.definition_id \
                 LEFT JOIN coordinate_results r ON r.coordinate_id = e.id \
                 WHERE e.workset_id = ?1 AND e.definition_id = ?2 \
                   AND e.state IN ('success', 'failed') AND e.result_path IS NOT NULL \
                 ORDER BY t.selector, e.family_key, e.repetition",
            )
            .map_err(|error| error.to_string())?;
        let mut tasks = HashMap::<String, (String, String)>::new();
        let points = statement
            .query_map((workset.id, task.id), |row| {
                let coordinate_id = row.get::<_, i64>(0)?;
                let task_name = row.get::<_, String>(1)?;
                let (task_id, task_label) = if let Some(task) = tasks.get(&task_name) {
                    task.clone()
                } else {
                    let task = (
                        public_id(&[digest, &task_name]),
                        short_name(&task_name).to_owned(),
                    );
                    tasks.insert(task_name.clone(), task.clone());
                    task
                };
                let model = row.get::<_, String>(4)?;
                let input_tokens = row.get(10)?;
                let cached_input_tokens = row.get(11)?;
                let output_tokens = row.get(12)?;
                let cost_usd = row.get::<_, Option<f64>>(15)?.or_else(|| {
                    estimated_cost_usd(&model, input_tokens, cached_input_tokens, output_tokens)
                });
                Ok(ResultPoint {
                    id: case_id(digest, coordinate_id),
                    task_id,
                    task_label,
                    task_name,
                    state: row.get(2)?,
                    harness: row.get(3)?,
                    model,
                    thinking: row.get(5)?,
                    repetition: row.get(6)?,
                    status: row.get(8)?,
                    outcome: row.get(9)?,
                    duration_ms: row.get(7)?,
                    input_tokens,
                    cached_input_tokens,
                    output_tokens,
                    reasoning_output_tokens: row.get(13)?,
                    total_tokens: row.get(14)?,
                    cost_usd,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(Some(WorksetResults {
            schema_version: API_SCHEMA_VERSION,
            observed_at_ms: now_ms()?,
            workset_id: workset.digest,
            points,
        }))
    }

    pub(crate) fn index_result(&self, result_path: &Path) -> Result<(), String> {
        let retained_path = result_path.to_string_lossy().into_owned();
        let result_path = result_path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let Some(evidence) = read_outcome(&result_path)? else {
            return Ok(());
        };
        let connection = Connection::open(&self.ledger).map_err(|error| error.to_string())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        let coordinate_id = connection
            .query_row(
                "SELECT id FROM eval_tasks WHERE result_path = ?1 \
                 AND state IN ('success', 'failed')",
                [&retained_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(coordinate_id) = coordinate_id {
            insert_result(&connection, coordinate_id, &retained_path, &evidence)?;
        }
        Ok(())
    }

    pub(crate) fn evidence(result_path: &Path) -> Result<Option<CaseEvidence>, String> {
        read_result_evidence(result_path)
    }

    pub(crate) fn task(
        &self,
        workset_digest: &str,
        task_id: &str,
    ) -> Result<Option<TaskDetail>, String> {
        let connection = self.connection()?;
        let Some(workset) = find_workset(&connection, workset_digest)? else {
            return Ok(None);
        };
        let Some(task) = find_task(&connection, workset.id, workset_digest, task_id)? else {
            return Ok(None);
        };
        let now = now_ms()?;
        let mut coordinates = read_coordinates(&connection, workset.id, task.id)?;
        let mut treatments = Vec::<TreatmentDetail>::new();
        for coordinate in coordinates.drain(..) {
            let state = coordinate_state(&coordinate);
            let detail_id =
                result_path(&coordinate).map(|_| case_id(workset_digest, coordinate.id));
            let cell = CoordinateDetail {
                id: case_id(workset_digest, coordinate.id),
                repetition: coordinate.repetition,
                state,
                status: coordinate.status,
                outcome: coordinate.outcome,
                updated_at_ms: coordinate.finished_at_ms.or(coordinate.started_at_ms),
                duration_ms: coordinate.agent_duration_ms,
                message: coordinate.error.clone(),
                detail_id,
            };
            let treatment_id = public_id(&[workset_digest, &coordinate.family_key]);
            if let Some(row) = treatments.iter_mut().find(|row| row.id == treatment_id) {
                row.cells.push(cell);
            } else {
                treatments.push(TreatmentDetail {
                    id: treatment_id,
                    label: treatment_label(
                        &coordinate.harness,
                        &coordinate.model,
                        &coordinate.thinking,
                    ),
                    harness: coordinate.harness,
                    model: coordinate.model,
                    thinking: coordinate.thinking,
                    cells: vec![cell],
                });
            }
        }
        treatments.sort_by(|left, right| left.label.cmp(&right.label));
        for treatment in &mut treatments {
            treatment.cells.sort_by_key(|cell| cell.repetition);
        }
        Ok(Some(TaskDetail {
            schema_version: API_SCHEMA_VERSION,
            observed_at_ms: now,
            workset_id: workset.digest,
            task: TaskMatrix {
                id: task_id.to_owned(),
                label: short_name(&task.name).to_owned(),
                name: task.name,
                digest: task.digest,
                treatments,
            },
        }))
    }

    pub(crate) fn task_outcomes(
        &self,
        workset_digest: &str,
        task_id: &str,
        cursor: usize,
        limit: usize,
    ) -> Result<Option<TaskOutcomesPage>, String> {
        let connection = self.connection()?;
        let Some(workset) = find_workset(&connection, workset_digest)? else {
            return Ok(None);
        };
        let Some(task) = find_task(&connection, workset.id, workset_digest, task_id)? else {
            return Ok(None);
        };
        let coordinates = read_coordinates(&connection, workset.id, task.id)?
            .into_iter()
            .filter(|coordinate| {
                matches!(coordinate.state.as_str(), "success" | "failed")
                    && result_path(coordinate).is_some()
            })
            .collect::<Vec<_>>();
        let total = coordinates.len();
        let mut outcomes = Vec::with_capacity(limit.min(total.saturating_sub(cursor)));
        for coordinate in coordinates.into_iter().skip(cursor).take(limit) {
            let retained = coordinate.status.is_some() || coordinate.outcome.is_some();
            let evidence = if retained {
                None
            } else {
                self.outcome_for(&coordinate)?
            };
            outcomes.push(CoordinateOutcome {
                id: case_id(workset_digest, coordinate.id),
                status: coordinate
                    .status
                    .or_else(|| evidence.as_ref().and_then(|value| value.status.clone())),
                outcome: coordinate
                    .outcome
                    .or_else(|| evidence.and_then(|value| value.outcome)),
            });
        }
        let loaded = cursor.saturating_add(outcomes.len());
        Ok(Some(TaskOutcomesPage {
            schema_version: API_SCHEMA_VERSION,
            observed_at_ms: now_ms()?,
            workset_id: workset.digest,
            task_id: task_id.to_owned(),
            total,
            next_cursor: (loaded < total).then_some(loaded),
            outcomes,
        }))
    }

    pub(crate) fn case(&self, id: &str) -> Result<Option<CaseEvidence>, String> {
        let connection = self.connection()?;
        for workset in read_worksets(&connection)? {
            let mut statement = connection
                .prepare(
                    "SELECT id, result_path FROM eval_tasks \
                     WHERE workset_id = ?1 AND result_path IS NOT NULL",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([workset.id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        PathBuf::from(row.get::<_, String>(1)?),
                    ))
                })
                .map_err(|error| error.to_string())?;
            for row in rows {
                let (coordinate_id, result_path) = row.map_err(|error| error.to_string())?;
                if case_id(&workset.digest, coordinate_id) == id {
                    return read_result_evidence(&result_path);
                }
            }
        }
        Ok(None)
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection = Connection::open_with_flags(
            &self.ledger,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error.to_string())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    fn outcome_for(&self, coordinate: &CoordinateRow) -> Result<Option<CaseEvidence>, String> {
        let Some(result_path) = result_path(coordinate) else {
            return Ok(None);
        };
        let result = match result_path.canonicalize() {
            Ok(result) => result,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        read_outcome(&result)
    }
}

const fn result_path(coordinate: &CoordinateRow) -> Option<&PathBuf> {
    coordinate.result_path.as_ref()
}

fn read_worksets(connection: &Connection) -> Result<Vec<WorksetRow>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, profile, digest, created_at_ms FROM worksets \
             ORDER BY created_at_ms DESC, id DESC",
        )
        .map_err(|error| error.to_string())?;
    statement
        .query_map([], |row| {
            Ok(WorksetRow {
                id: row.get(0)?,
                profile: row.get(1)?,
                digest: row.get(2)?,
                created_at_ms: row.get(3)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn find_workset(connection: &Connection, digest: &str) -> Result<Option<WorksetRow>, String> {
    connection
        .query_row(
            "SELECT id, profile, digest, created_at_ms FROM worksets WHERE digest = ?1",
            [digest],
            |row| {
                Ok(WorksetRow {
                    id: row.get(0)?,
                    profile: row.get(1)?,
                    digest: row.get(2)?,
                    created_at_ms: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())
}

fn read_task_overviews(
    connection: &Connection,
    workset: &WorksetRow,
) -> Result<Vec<TaskOverview>, String> {
    let mut statement = connection
        .prepare(
            "SELECT d.selector, d.digest, COUNT(DISTINCT e.family_key), COUNT(e.id), \
                    COALESCE(SUM(e.state = 'unclaimed'), 0), \
                    COALESCE(SUM(e.state = 'running'), 0), \
                    COALESCE(SUM(e.state = 'success'), 0), \
                    COALESCE(SUM(e.state = 'failed'), 0) \
             FROM task_definitions d \
             LEFT JOIN eval_tasks e \
               ON e.workset_id = d.workset_id AND e.definition_id = d.id \
             WHERE d.workset_id = ?1 \
             GROUP BY d.id, d.selector, d.digest \
             ORDER BY d.selector",
        )
        .map_err(|error| error.to_string())?;
    statement
        .query_map([workset.id], |row| {
            let name = row.get::<_, String>(0)?;
            Ok(TaskOverview {
                id: public_id(&[&workset.digest, &name]),
                label: short_name(&name).to_owned(),
                name,
                digest: row.get(1)?,
                treatment_count: nonnegative_count(row, 2)?,
                summary: summary_from_row(row, 3)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn summary_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<EvalSummary> {
    Ok(EvalSummary {
        total: nonnegative_count(row, offset)?,
        unclaimed: nonnegative_count(row, offset + 1)?,
        running: nonnegative_count(row, offset + 2)?,
        success: nonnegative_count(row, offset + 3)?,
        failed: nonnegative_count(row, offset + 4)?,
    })
}

fn nonnegative_count(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn find_task(
    connection: &Connection,
    workset_id: i64,
    workset_digest: &str,
    public_task_id: &str,
) -> Result<Option<TaskRow>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, selector, digest FROM task_definitions WHERE workset_id = ?1 ORDER BY id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([workset_id], |row| {
            Ok(TaskRow {
                id: row.get(0)?,
                name: row.get(1)?,
                digest: row.get(2)?,
            })
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let task = row.map_err(|error| error.to_string())?;
        if public_id(&[workset_digest, &task.name]) == public_task_id {
            return Ok(Some(task));
        }
    }
    Ok(None)
}

fn read_coordinates(
    connection: &Connection,
    workset_id: i64,
    task_id: i64,
) -> Result<Vec<CoordinateRow>, String> {
    let mut statement = connection
        .prepare(
            "SELECT e.id, e.family_key, e.harness, e.model, e.thinking, \
                    e.repetition, e.state, e.result_path, e.started_at_ms, \
                    e.finished_at_ms, e.error, \
                    r.status, r.outcome, r.agent_duration_ms \
             FROM eval_tasks e INDEXED BY eval_tasks_definition \
             LEFT JOIN coordinate_results r ON r.coordinate_id = e.id \
             WHERE e.workset_id = ?1 AND e.definition_id = ?2 \
             ORDER BY e.family_key, e.repetition",
        )
        .map_err(|error| error.to_string())?;
    let coordinates = statement
        .query_map((workset_id, task_id), |row| {
            Ok(CoordinateRow {
                id: row.get(0)?,
                family_key: row.get(1)?,
                harness: row.get(2)?,
                model: row.get(3)?,
                thinking: row.get(4)?,
                repetition: row.get(5)?,
                state: row.get(6)?,
                result_path: row.get::<_, Option<String>>(7)?.map(PathBuf::from),
                started_at_ms: row.get(8)?,
                finished_at_ms: row.get(9)?,
                error: row.get(10)?,
                status: row.get(11)?,
                outcome: row.get(12)?,
                agent_duration_ms: row.get(13)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(coordinates)
}

fn coordinate_state(coordinate: &CoordinateRow) -> &'static str {
    match coordinate.state.as_str() {
        "unclaimed" => "unclaimed",
        "running" => "running",
        "success" => "success",
        "failed" => "failed",
        _ => "failed",
    }
}

fn retained_result_passed(state: &str, status: Option<&str>, outcome: Option<&str>) -> bool {
    match status {
        Some(status) => status == "passed",
        None => match outcome {
            Some(outcome) => outcome == "passed",
            None => state == "success",
        },
    }
}

fn median_i64(values: &mut [i64]) -> Option<f64> {
    values.sort_unstable();
    let middle = values.len().checked_div(2)?;
    if values.len().is_multiple_of(2) {
        let left = *values.get(middle.checked_sub(1)?)? as f64;
        let right = *values.get(middle)? as f64;
        Some((left + right) / 2.0)
    } else {
        values.get(middle).map(|value| *value as f64)
    }
}

fn median_f64(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let middle = values.len().checked_div(2)?;
    if values.len().is_multiple_of(2) {
        Some((values.get(middle.checked_sub(1)?)? + values.get(middle)?) / 2.0)
    } else {
        values.get(middle).copied()
    }
}

fn read_result_evidence(result_path: &Path) -> Result<Option<CaseEvidence>, String> {
    let result = match result_path.canonicalize() {
        Ok(result) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    read_evidence(&result)
}

fn read_evidence(result: &Path) -> Result<Option<CaseEvidence>, String> {
    let Some(events) = events_path(result) else {
        return Ok(None);
    };
    let file = match File::open(events) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut evidence = empty_evidence();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| error.to_string())?;
        let kind = retained_event_kind(line.as_bytes())?;
        if !matches!(
            kind,
            "attempt_started" | "verifier_output" | "completed" | "failed"
        ) {
            continue;
        }
        if line.len() > MAX_EVENT_LINE_BYTES {
            return Err("retained evaluation event exceeded the API limit".to_owned());
        }
        let event: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        match kind {
            "attempt_started" => {
                evidence.task_name = event
                    .pointer("/attempt/task_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                evidence.prompt = event
                    .pointer("/payload/prompt")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "verifier_output" => {
                evidence.verifier_stdout = event
                    .pointer("/payload/stdout")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                evidence.verifier_stderr = event
                    .pointer("/payload/stderr")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "completed" | "failed" => apply_terminal_payload(&mut evidence, &event["payload"]),
            _ => {}
        }
    }
    Ok((evidence.status.is_some() || evidence.outcome.is_some()).then_some(evidence))
}

fn read_outcome(result: &Path) -> Result<Option<CaseEvidence>, String> {
    let Some(events) = events_path(result) else {
        return Ok(None);
    };
    let mut file = match File::open(&events) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    let start = length.saturating_sub(OUTCOME_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| error.to_string())?;
    let mut tail = Vec::with_capacity(
        usize::try_from(length.saturating_sub(start)).map_err(|error| error.to_string())?,
    );
    file.read_to_end(&mut tail)
        .map_err(|error| error.to_string())?;
    let first_complete_line = if start == 0 {
        0
    } else {
        tail.iter()
            .position(|byte| *byte == b'\n')
            .map_or(tail.len(), |position| position + 1)
    };
    for line in tail[first_complete_line..]
        .split(|byte| *byte == b'\n')
        .rev()
        .filter(|line| !line.is_empty())
    {
        let kind = retained_event_kind(line)?;
        if !matches!(kind, "completed" | "failed") {
            continue;
        }
        if line.len() > MAX_EVENT_LINE_BYTES {
            return Err("retained evaluation event exceeded the API limit".to_owned());
        }
        let event: Value = serde_json::from_slice(line).map_err(|error| error.to_string())?;
        let mut evidence = empty_evidence();
        apply_terminal_payload(&mut evidence, &event["payload"]);
        return Ok(Some(evidence));
    }
    read_evidence(result)
}

#[derive(Deserialize)]
struct RetainedEventKind<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
}

fn retained_event_kind(line: &[u8]) -> Result<&str, String> {
    serde_json::from_slice::<RetainedEventKind<'_>>(line)
        .map(|event| event.kind)
        .map_err(|error| error.to_string())
}

fn events_path(result: &Path) -> Option<PathBuf> {
    if result.is_dir() {
        Some(result.join("events.jsonl"))
    } else if result
        .file_name()
        .is_some_and(|name| name == "events.jsonl")
    {
        Some(result.to_path_buf())
    } else if result.is_file() {
        result.parent().map(|parent| parent.join("events.jsonl"))
    } else {
        None
    }
}

const fn empty_evidence() -> CaseEvidence {
    CaseEvidence {
        schema_version: API_SCHEMA_VERSION,
        task_name: None,
        prompt: None,
        status: None,
        outcome: None,
        environment: None,
        model: None,
        effort: None,
        final_message: None,
        tool_calls: None,
        usage: None,
        cost_usd: None,
        verifier: None,
        exception: None,
        timing: None,
        verifier_stdout: None,
        verifier_stderr: None,
    }
}

fn apply_terminal_payload(evidence: &mut CaseEvidence, payload: &Value) {
    evidence.task_name = payload
        .get("task_name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| evidence.task_name.take());
    evidence.status = payload
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_owned);
    evidence.outcome = payload
        .get("outcome")
        .and_then(Value::as_str)
        .map(str::to_owned);
    evidence.environment = payload
        .get("environment")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let agent = payload.get("agent").filter(|value| value.is_object());
    evidence.model = agent
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    evidence.effort = agent
        .and_then(|value| value.get("effort"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            payload
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    evidence.final_message = agent
        .and_then(|value| value.get("final_message"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    evidence.tool_calls = agent
        .and_then(|value| value.get("tool_calls"))
        .and_then(Value::as_u64);
    evidence.usage = agent.and_then(|value| value.get("usage")).cloned();
    evidence.cost_usd = agent
        .and_then(|value| value.get("cost_usd"))
        .and_then(decimal_value)
        .or_else(|| {
            agent
                .and_then(|value| value.pointer("/metadata/estimated_cost/usd"))
                .and_then(decimal_value)
        });
    evidence.verifier = payload
        .get("verifier")
        .filter(|value| value.is_object())
        .cloned();
    evidence.exception = payload
        .get("exception")
        .filter(|value| !value.is_null())
        .cloned();
    evidence.timing = payload
        .get("timing")
        .filter(|value| value.is_object())
        .cloned();
}

fn insert_result(
    connection: &Connection,
    coordinate_id: i64,
    result_path: &str,
    evidence: &CaseEvidence,
) -> Result<(), String> {
    let usage = evidence.usage.as_ref();
    connection
        .execute(
            "INSERT INTO coordinate_results( \
                coordinate_id, result_path, status, outcome, input_tokens, cached_input_tokens, \
                output_tokens, reasoning_output_tokens, total_tokens, cost_usd, \
                agent_duration_ms \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(coordinate_id) DO UPDATE SET \
                result_path = excluded.result_path, status = excluded.status, \
                outcome = excluded.outcome, \
                input_tokens = excluded.input_tokens, \
                cached_input_tokens = excluded.cached_input_tokens, \
                output_tokens = excluded.output_tokens, \
                reasoning_output_tokens = excluded.reasoning_output_tokens, \
                total_tokens = excluded.total_tokens, cost_usd = excluded.cost_usd, \
                agent_duration_ms = excluded.agent_duration_ms",
            params![
                coordinate_id,
                result_path,
                evidence.status,
                evidence.outcome,
                usage.and_then(|value| integer_field(value, "input_tokens")),
                usage.and_then(|value| integer_field(value, "cached_input_tokens")),
                usage.and_then(|value| integer_field(value, "output_tokens")),
                usage.and_then(|value| integer_field(value, "reasoning_output_tokens")),
                usage.and_then(|value| integer_field(value, "total_tokens")),
                evidence.cost_usd,
                agent_duration_ms(evidence.timing.as_ref()),
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn integer_field(value: &Value, field: &str) -> Option<i64> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .or_else(|| {
            value
                .get(field)
                .and_then(Value::as_u64)
                .and_then(|value| i64::try_from(value).ok())
        })
        .or_else(|| {
            value
                .get(field)
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
        })
}

fn decimal_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn agent_duration_ms(timing: Option<&Value>) -> Option<i64> {
    let execution = timing?.get("agent_execution")?;
    let started = DateTime::parse_from_rfc3339(execution.get("started_at")?.as_str()?).ok()?;
    let finished = DateTime::parse_from_rfc3339(execution.get("finished_at")?.as_str()?).ok()?;
    let duration = finished.signed_duration_since(started).num_milliseconds();
    (duration >= 0).then_some(duration)
}

fn estimated_cost_usd(
    model: &str,
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
) -> Option<f64> {
    let model = model.parse::<Model>().ok()?;
    let input_tokens = u64::try_from(input_tokens?).ok()?;
    let cached_input_tokens = u64::try_from(cached_input_tokens.unwrap_or_default()).ok()?;
    let output_tokens = u64::try_from(output_tokens?).ok()?;
    let usage = Usage {
        input_tokens,
        input_tokens_details: Some(InputTokenDetails {
            cached_tokens: cached_input_tokens,
            cache_write_tokens: 0,
        }),
        output_tokens,
        total_tokens: input_tokens.saturating_add(output_tokens),
        ..Usage::default()
    };
    estimate_for_model(&usage, model, ServiceTier::Standard)
        .amount()
        .decimal()
        .parse()
        .ok()
}

fn treatment_label(harness: &str, model: &str, thinking: &str) -> String {
    [harness, model, thinking]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

const fn add_summary(total: &mut EvalSummary, summary: &EvalSummary) {
    total.total += summary.total;
    total.unclaimed += summary.unclaimed;
    total.running += summary.running;
    total.success += summary.success;
    total.failed += summary.failed;
}

fn short_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn case_id(workset_digest: &str, coordinate_id: i64) -> String {
    public_id(&[workset_digest, &coordinate_id.to_string()])
}

fn public_id(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())[..24].to_owned()
}

fn now_ms() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    i64::try_from(millis).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write as _};

    use serde_json::json;
    use tempfile::tempdir;

    use super::{MAX_EVENT_LINE_BYTES, agent_duration_ms, read_evidence, read_outcome};

    #[test]
    fn agent_duration_uses_retained_execution_phase() {
        let timing = json!({
            "started_at": "2026-08-17T22:29:47.063935980Z",
            "finished_at": "2026-08-17T22:29:50.535579014Z",
            "agent_execution": {
                "started_at": "2026-08-17T22:29:47.160886122Z",
                "finished_at": "2026-08-17T22:29:50.313306533Z"
            }
        });
        assert_eq!(agent_duration_ms(Some(&timing)), Some(3_152));
    }

    #[test]
    fn evidence_reader_skips_oversized_unmaterialized_events() {
        let result = tempdir().unwrap();
        let mut events = File::create(result.path().join("events.jsonl")).unwrap();
        write!(events, "{{\"type\":\"agent\",\"payload\":\"").unwrap();
        events
            .write_all(&vec![b'x'; MAX_EVENT_LINE_BYTES + 1])
            .unwrap();
        writeln!(events, "\"}}").unwrap();
        writeln!(
            events,
            "{{\"type\":\"completed\",\"payload\":{{\"status\":\"success\",\"outcome\":\"pass\"}}}}"
        )
        .unwrap();
        drop(events);

        let evidence = read_evidence(result.path()).unwrap().unwrap();
        assert_eq!(evidence.status.as_deref(), Some("success"));
        assert_eq!(evidence.outcome.as_deref(), Some("pass"));

        let outcome = read_outcome(result.path()).unwrap().unwrap();
        assert_eq!(outcome.status.as_deref(), Some("success"));
        assert_eq!(outcome.outcome.as_deref(), Some("pass"));
    }
}
