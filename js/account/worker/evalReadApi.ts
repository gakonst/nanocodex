import { validImportKey, type EvalStorageEnv } from "./evalCoordinator.ts";

const API_SCHEMA_VERSION = 5;
const JSON_HEADERS = {
  "cache-control": "no-store",
  "content-type": "application/json; charset=utf-8",
  "x-content-type-options": "nosniff",
};

type Summary = {
  total: number;
  unclaimed: number;
  running: number;
  success: number;
  failed: number;
};

type WorksetRow = Summary & {
  id: number;
  profile: string;
  digest: string;
  created_at_ms: number;
  task_count: number;
};

type WorksetTaskRow = Summary & {
  workset_id: number;
  profile: string;
  workset_digest: string;
  created_at_ms: number;
  task_id: number | null;
  public_id: string | null;
  name: string | null;
  task_digest: string | null;
  treatment_count: number;
};

type TaskSnapshotRow = {
  workset_id: number;
  profile: string;
  workset_digest: string;
  created_at_ms: number;
  workset_task_count: number;
  workset_total: number;
  workset_unclaimed: number;
  workset_running: number;
  workset_success: number;
  workset_failed: number;
  task_id: number;
  public_id: string;
  name: string;
  task_digest: string;
  treatment_count: number;
  task_total: number;
  task_unclaimed: number;
  task_running: number;
  task_success: number;
  task_failed: number;
};

type AnalyticsRow = {
  harness: string;
  model: string;
  thinking: string;
  passed: number;
  completed: number;
  task_count: number;
  median_output_tokens: number | null;
  output_samples: number;
  median_duration_ms: number | null;
  duration_samples: number;
  median_cost_usd: number | null;
  cost_samples: number;
};

type CoordinateRow = {
  id: number;
  public_id: string;
  task_public_id: string;
  task_name: string;
  task_digest: string;
  family_key: string;
  harness: string;
  model: string;
  thinking: string;
  repetition: number;
  state: "unclaimed" | "running" | "success" | "failed";
  started_at_ms: number | null;
  finished_at_ms: number | null;
  error: string | null;
  case_key: string | null;
  status: string | null;
  outcome: string | null;
  input_tokens: number | null;
  cached_input_tokens: number | null;
  output_tokens: number | null;
  reasoning_output_tokens: number | null;
  total_tokens: number | null;
  cost_usd: number | null;
  agent_duration_ms: number | null;
};

export async function routeEvalRead(
  request: Request,
  env: EvalStorageEnv,
  url: URL,
  context?: ExecutionContext,
): Promise<Response | null> {
  if (request.method !== "GET") return null;
  if (url.pathname !== "/v1/status" &&
    url.pathname !== "/v1/task-package" &&
    url.pathname !== "/api/evals" &&
    !url.pathname.startsWith("/api/evals/")) return null;
  if (!env.EVALS_DB || !env.EVALS_ARTIFACTS) {
    return json({ error: "evaluation API is not configured" }, 503);
  }
  try {
    if (url.pathname === "/v1/status") return status(env.EVALS_DB, url);
    if (url.pathname === "/v1/task-package") return taskPackage(request, env, url, context);
    if (url.pathname === "/api/evals") return overview(env.EVALS_DB);
    if (url.pathname === "/api/evals/cluster") return cluster(env.EVALS_DB);
    const caseMatch = url.pathname.match(/^\/api\/evals\/cases\/([^/]+)$/);
    if (caseMatch) return evalCase(request, env, decodeURIComponent(caseMatch[1]), context);
    const analytics = url.pathname.match(/^\/api\/evals\/worksets\/([^/]+)\/analytics$/);
    if (analytics) return worksetAnalytics(env.EVALS_DB, decodeURIComponent(analytics[1]));
    const outcomes = url.pathname.match(/^\/api\/evals\/worksets\/([^/]+)\/tasks\/([^/]+)\/outcomes$/);
    if (outcomes) return taskOutcomes(env.EVALS_DB, decodeURIComponent(outcomes[1]), decodeURIComponent(outcomes[2]), url);
    const task = url.pathname.match(/^\/api\/evals\/worksets\/([^/]+)\/tasks\/([^/]+)$/);
    if (task) return taskSnapshot(env.EVALS_DB, decodeURIComponent(task[1]), decodeURIComponent(task[2]));
    const workset = url.pathname.match(/^\/api\/evals\/worksets\/([^/]+)$/);
    if (workset) return worksetDetail(env.EVALS_DB, decodeURIComponent(workset[1]));
    return json({ error: "not_found" }, 404);
  } catch (cause) {
    console.error({
      type: "evaluation.read_failed",
      error_kind: cause instanceof Error ? cause.name : typeof cause,
    });
    return json({ error: "evaluation API read failed" }, 500);
  }
}

async function taskPackage(
  request: Request,
  env: EvalStorageEnv,
  url: URL,
  context?: ExecutionContext,
): Promise<Response> {
  const key = url.searchParams.get("key");
  if (!validImportKey(key) || !key.startsWith("tasks/")) {
    return json({ error: "invalid task package key" }, 400);
  }
  return cachedImmutable(request, context, async () => {
    const object = await env.EVALS_ARTIFACTS!.get(key);
    if (!object) return json({ error: "task package was not found" }, 404);
    return new Response(object.body, {
      headers: {
        "cache-control": "public, max-age=31536000, immutable",
        "content-length": String(object.size),
        "content-type": "application/x-tar+zstd",
        etag: object.httpEtag,
        "x-content-type-options": "nosniff",
      },
    });
  });
}

async function overview(db: D1Database): Promise<Response> {
  const worksets = await readWorksets(db);
  return json({
    schemaVersion: API_SCHEMA_VERSION,
    observedAtMs: Date.now(),
    summary: worksets.reduce((total, row) => addSummary(total, row), emptySummary()),
    worksets: worksets.map(publicWorkset),
  });
}

async function worksetDetail(db: D1Database, digest: string): Promise<Response> {
  const rows = await db.prepare(
    `SELECT w.id AS workset_id, w.profile, w.digest AS workset_digest, w.created_at_ms,
      d.id AS task_id, d.public_id, d.name, d.digest AS task_digest,
      COUNT(DISTINCT e.family_key) AS treatment_count, ${summaryColumns("e")}
    FROM worksets w
    LEFT JOIN task_definitions d ON d.workset_id = w.id
    LEFT JOIN eval_tasks e ON e.workset_id = w.id AND e.definition_id = d.id
    WHERE w.state = 'ready' AND w.digest = ?1
    GROUP BY w.id, d.id
    ORDER BY d.id`,
  ).bind(digest).all<WorksetTaskRow>();
  const first = rows.results[0];
  if (!first) return json({ error: "evaluation workset was not found" }, 404);
  const taskRows = rows.results.filter((row): row is WorksetTaskRow & {
    task_id: number;
    public_id: string;
    name: string;
    task_digest: string;
  } => row.task_id != null && row.public_id != null && row.name != null && row.task_digest != null);
  const worksetSummary = taskRows.reduce(
    (total, row) => addSummary(total, row),
    emptySummary(),
  );
  return json({
    schemaVersion: API_SCHEMA_VERSION,
    observedAtMs: Date.now(),
    workset: {
      id: first.workset_digest,
      profile: first.profile,
      digest: first.workset_digest,
      createdAtMs: first.created_at_ms,
      taskCount: taskRows.length,
      summary: worksetSummary,
    },
    tasks: taskRows.map((task) => ({
      id: task.public_id,
      name: task.name,
      label: shortName(task.name),
      digest: task.task_digest,
      treatmentCount: task.treatment_count,
      summary: summary(task),
    })),
  });
}

async function taskSnapshot(db: D1Database, digest: string, taskId: string): Promise<Response> {
  const metadata = await db.prepare(
    `WITH target AS MATERIALIZED (
      SELECT w.id AS workset_id, w.profile, w.digest AS workset_digest, w.created_at_ms,
        d.id AS task_id, d.public_id, d.name, d.digest AS task_digest
      FROM worksets w
      JOIN task_definitions d ON d.workset_id = w.id
      WHERE w.state = 'ready' AND w.digest = ?1 AND d.public_id = ?2
    ), task_rows AS MATERIALIZED (
      SELECT e.definition_id, COUNT(DISTINCT e.family_key) AS treatment_count,
        ${summaryColumns("e")}
      FROM eval_tasks e
      JOIN target ON target.workset_id = e.workset_id
      GROUP BY e.definition_id
    ), board AS (
      SELECT COALESCE(SUM(total), 0) AS total,
        COALESCE(SUM(unclaimed), 0) AS unclaimed,
        COALESCE(SUM(running), 0) AS running,
        COALESCE(SUM(success), 0) AS success,
        COALESCE(SUM(failed), 0) AS failed
      FROM task_rows
    )
    SELECT target.*,
      (SELECT COUNT(*) FROM task_definitions d WHERE d.workset_id = target.workset_id) AS workset_task_count,
      board.total AS workset_total, board.unclaimed AS workset_unclaimed,
      board.running AS workset_running, board.success AS workset_success,
      board.failed AS workset_failed,
      COALESCE(task_rows.treatment_count, 0) AS treatment_count,
      COALESCE(task_rows.total, 0) AS task_total,
      COALESCE(task_rows.unclaimed, 0) AS task_unclaimed,
      COALESCE(task_rows.running, 0) AS task_running,
      COALESCE(task_rows.success, 0) AS task_success,
      COALESCE(task_rows.failed, 0) AS task_failed
    FROM target
    CROSS JOIN board
    LEFT JOIN task_rows ON task_rows.definition_id = target.task_id`,
  ).bind(digest, taskId).first<TaskSnapshotRow>();
  if (!metadata) return json({ error: "evaluation task was not found" }, 404);
  const coordinates = await readCoordinates(db, metadata.workset_id, metadata.task_id);
  const treatments = new Map<string, {
    id: string;
    label: string;
    harness: string;
    model: string;
    thinking: string;
    cells: Array<Record<string, unknown>>;
  }>();
  for (const coordinate of coordinates) {
    let treatment = treatments.get(coordinate.family_key);
    if (!treatment) {
      const id = await publicId(digest, coordinate.family_key);
      treatment = {
        id,
        label: [coordinate.harness, coordinate.model, coordinate.thinking].filter(Boolean).join(" · "),
        harness: coordinate.harness,
        model: coordinate.model,
        thinking: coordinate.thinking,
        cells: [],
      };
      treatments.set(coordinate.family_key, treatment);
    }
    treatment.cells.push({
      id: coordinate.public_id,
      repetition: coordinate.repetition,
      state: coordinate.state,
      status: coordinate.status,
      outcome: coordinate.outcome,
      updatedAtMs: coordinate.finished_at_ms ?? coordinate.started_at_ms,
      durationMs: duration(coordinate),
      message: coordinate.error,
      detailId: coordinate.case_key ? coordinate.public_id : null,
    });
  }
  for (const treatment of treatments.values()) {
    treatment.cells.sort((left, right) => Number(left.repetition) - Number(right.repetition));
  }
  return json({
    schemaVersion: API_SCHEMA_VERSION,
    observedAtMs: Date.now(),
    worksetId: digest,
    workset: {
      id: metadata.workset_digest,
      profile: metadata.profile,
      digest: metadata.workset_digest,
      createdAtMs: metadata.created_at_ms,
      taskCount: Number(metadata.workset_task_count),
      summary: prefixedSummary(metadata, "workset"),
    },
    taskSummary: {
      id: metadata.public_id,
      name: metadata.name,
      label: shortName(metadata.name),
      digest: metadata.task_digest,
      treatmentCount: Number(metadata.treatment_count),
      summary: prefixedSummary(metadata, "task"),
    },
    task: {
      id: metadata.public_id,
      name: metadata.name,
      label: shortName(metadata.name),
      digest: metadata.task_digest,
      treatments: [...treatments.values()].sort((left, right) => left.label.localeCompare(right.label)),
    },
    points: coordinates
      .filter(({ state }) => state === "success" || state === "failed")
      .map(publicResultPoint),
  });
}

async function taskOutcomes(
  db: D1Database,
  digest: string,
  taskId: string,
  url: URL,
): Promise<Response> {
  const found = await findWorksetAndTask(db, digest, taskId);
  if (!found) return json({ error: "evaluation task was not found" }, 404);
  const cursor = boundedInteger(url.searchParams.get("cursor"), 0, 1_000_000, 0);
  const limit = boundedInteger(url.searchParams.get("limit"), 1, 32, 8);
  const total = await db.prepare(
    "SELECT COUNT(*) AS count FROM eval_tasks e JOIN coordinate_results r ON r.coordinate_id = e.id WHERE e.definition_id = ?1 AND e.state IN ('success', 'failed') AND r.case_key IS NOT NULL",
  ).bind(found.taskId).first<{ count: number }>();
  const outcomes = await db.prepare(
    "SELECT e.public_id AS id, r.status, r.outcome FROM eval_tasks e JOIN coordinate_results r ON r.coordinate_id = e.id WHERE e.definition_id = ?1 AND e.state IN ('success', 'failed') AND r.case_key IS NOT NULL ORDER BY e.id LIMIT ?2 OFFSET ?3",
  ).bind(found.taskId, limit, cursor).all<{ id: string; status: string | null; outcome: string | null }>();
  const loaded = cursor + outcomes.results.length;
  return json({
    schemaVersion: API_SCHEMA_VERSION,
    observedAtMs: Date.now(),
    worksetId: digest,
    taskId,
    total: total?.count ?? 0,
    nextCursor: loaded < (total?.count ?? 0) ? loaded : null,
    outcomes: outcomes.results,
  });
}

async function worksetAnalytics(db: D1Database, digest: string): Promise<Response> {
  const rows = await db.prepare(
    `WITH target AS MATERIALIZED (
      SELECT id FROM worksets WHERE state = 'ready' AND digest = ?1
    ), task_total AS MATERIALIZED (
      SELECT COUNT(*) AS task_count
      FROM task_definitions d JOIN target ON target.id = d.workset_id
    ), ranked AS (
      SELECT e.harness, e.model, e.thinking, e.state, task_total.task_count,
        CASE WHEN r.status = 'passed' OR (
          r.status IS NULL AND (
            r.outcome = 'passed' OR (r.outcome IS NULL AND e.state = 'success')
          )
        ) THEN 1 ELSE 0 END AS passed,
        r.output_tokens,
        CASE WHEN r.agent_duration_ms IS NULL
          THEN NULL ELSE MAX(r.agent_duration_ms, 0) END AS duration_ms,
        r.cost_usd,
        COUNT(r.output_tokens) OVER (
          PARTITION BY e.harness, e.model, e.thinking
          ORDER BY r.output_tokens ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS output_rank,
        COUNT(r.output_tokens) OVER (
          PARTITION BY e.harness, e.model, e.thinking
        ) AS output_count,
        COUNT(r.agent_duration_ms) OVER (
          PARTITION BY e.harness, e.model, e.thinking
          ORDER BY r.agent_duration_ms ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS duration_rank,
        COUNT(r.agent_duration_ms) OVER (
          PARTITION BY e.harness, e.model, e.thinking
        ) AS duration_count,
        COUNT(r.cost_usd) OVER (
          PARTITION BY e.harness, e.model, e.thinking
          ORDER BY r.cost_usd ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS cost_rank,
        COUNT(r.cost_usd) OVER (
          PARTITION BY e.harness, e.model, e.thinking
        ) AS cost_count
      FROM target
      JOIN eval_tasks e ON e.workset_id = target.id
      CROSS JOIN task_total
      LEFT JOIN coordinate_results r ON r.coordinate_id = e.id
      WHERE e.state IN ('success', 'failed')
    )
    SELECT harness, model, thinking, SUM(passed) AS passed, COUNT(*) AS completed,
      task_count,
      AVG(CASE WHEN output_count > 0 AND output_rank IN (
        (output_count + 1) / 2, (output_count + 2) / 2
      ) THEN output_tokens END) AS median_output_tokens,
      MAX(output_count) AS output_samples,
      AVG(CASE WHEN duration_count > 0 AND duration_rank IN (
        (duration_count + 1) / 2, (duration_count + 2) / 2
      ) THEN duration_ms END) AS median_duration_ms,
      MAX(duration_count) AS duration_samples,
      AVG(CASE WHEN cost_count > 0 AND cost_rank IN (
        (cost_count + 1) / 2, (cost_count + 2) / 2
      ) THEN cost_usd END) AS median_cost_usd,
      MAX(cost_count) AS cost_samples
    FROM ranked
    GROUP BY harness, model, thinking
    ORDER BY harness, model, thinking`,
  ).bind(digest).all<AnalyticsRow>();
  let taskCount = rows.results[0]?.task_count;
  if (taskCount == null) {
    const workset = await findWorksetMetadata(db, digest);
    if (!workset) return json({ error: "evaluation workset was not found" }, 404);
    taskCount = workset.task_count;
  }
  return json({
    schemaVersion: API_SCHEMA_VERSION,
    observedAtMs: Date.now(),
    worksetId: digest,
    taskCount: Number(taskCount),
    points: rows.results.map((row) => ({
      harness: row.harness,
      model: row.model,
      thinking: row.thinking,
      passed: Number(row.passed),
      completed: Number(row.completed),
      medianOutputTokens: row.median_output_tokens,
      outputSamples: Number(row.output_samples),
      medianDurationMs: row.median_duration_ms,
      durationSamples: Number(row.duration_samples),
      medianCostUsd: row.median_cost_usd,
      costSamples: Number(row.cost_samples),
    })),
  });
}

async function evalCase(
  request: Request,
  env: EvalStorageEnv,
  id: string,
  context?: ExecutionContext,
): Promise<Response> {
  return cachedImmutable(request, context, async () => {
    const row = await env.EVALS_DB!.prepare(
      "SELECT r.case_key FROM coordinate_results r JOIN eval_tasks e ON e.id = r.coordinate_id WHERE e.public_id = ?1",
    ).bind(id).first<{ case_key: string | null }>();
    if (!row?.case_key) return json({ error: "evaluation case was not found" }, 404);
    const object = await env.EVALS_ARTIFACTS!.get(row.case_key);
    if (!object) return json({ error: "evaluation case was not found" }, 404);
    return new Response(object.body, {
      headers: {
        "cache-control": "public, max-age=31536000, immutable",
        "content-type": "application/json; charset=utf-8",
        etag: object.httpEtag,
        "x-content-type-options": "nosniff",
      },
    });
  });
}

async function cachedImmutable(
  request: Request,
  context: ExecutionContext | undefined,
  load: () => Promise<Response>,
): Promise<Response> {
  const edgeCache = typeof caches === "undefined"
    ? undefined
    : (caches as CacheStorage & { default: Cache }).default;
  const cacheKey = new Request(request.url, { method: "GET" });
  const cached = await edgeCache?.match(cacheKey);
  if (cached) return cached;
  const response = await load();
  if (response.ok && edgeCache && context) {
    context.waitUntil(edgeCache.put(cacheKey, response.clone()));
  }
  return response;
}

async function cluster(db: D1Database): Promise<Response> {
  const now = Date.now();
  const rows = await db.prepare(
    "SELECT observed_at_ms, payload_json FROM cluster_nodes WHERE expires_at_ms > ?1 ORDER BY id",
  ).bind(now).all<{ observed_at_ms: number; payload_json: string }>();
  const nodes = rows.results.flatMap((row) => {
    try {
      return [{ ...JSON.parse(row.payload_json), observedAtMs: row.observed_at_ms }];
    } catch {
      return [];
    }
  });
  return json({ schemaVersion: API_SCHEMA_VERSION, observedAtMs: now, nodes });
}

async function status(db: D1Database, url: URL): Promise<Response> {
  const requestedProfile = url.searchParams.get("profile");
  if (requestedProfile != null && (requestedProfile.length === 0 || requestedProfile.length > 512)) {
    return json({ error: "invalid profile" }, 400);
  }
  const workset = requestedProfile == null
    ? (await readWorksets(db))[0]
    : await findLatestProfile(db, requestedProfile);
  if (!workset && requestedProfile != null) {
    return json({ error: "evaluation profile was not found" }, 404);
  }
  if (!workset) return json({
    profile: "",
    digest: "",
    tasks: { unclaimed: 0, running: 0, success: 0, failed: 0 },
    workers: [],
    recent_attempts: { passed: 0, failed: 0, infrastructure_failed: 0, interrupted: 0, failures: [] },
    families: [],
  });
  const workers = await db.prepare(
    "SELECT DISTINCT worker FROM eval_tasks WHERE workset_id = ?1 AND state = 'running' AND worker IS NOT NULL ORDER BY worker",
  ).bind(workset.id).all<{ worker: string }>();
  const families = await db.prepare(
    `SELECT e.family_key AS id, d.selector AS task, e.harness, e.model, e.thinking, e.web_search, COUNT(*) AS desired, ${summaryColumns("e")} FROM eval_tasks e JOIN task_definitions d ON d.id = e.definition_id WHERE e.workset_id = ?1 GROUP BY e.family_key ORDER BY e.family_key`,
  ).bind(workset.id).all<Summary & { id: string; task: string; harness: string; model: string; thinking: string; web_search: number; desired: number }>();
  const since = Date.now() - 5 * 60 * 1_000;
  const attempts = await db.prepare(
    "SELECT worker, state, error, finished_at_ms FROM eval_attempts WHERE workset_id = ?1 AND finished_at_ms >= ?2 AND state != 'running' ORDER BY finished_at_ms DESC",
  ).bind(workset.id, since).all<{ worker: string; state: string; error: string | null; finished_at_ms: number }>();
  const recent = { passed: 0, failed: 0, infrastructure_failed: 0, interrupted: 0 };
  for (const attempt of attempts.results) {
    if (attempt.state in recent) recent[attempt.state as keyof typeof recent]++;
  }
  return json({
    profile: workset.profile,
    digest: workset.digest,
    tasks: summary(workset),
    workers: workers.results.map(({ worker }) => worker),
    recent_attempts: {
      ...recent,
      failures: attempts.results.filter(({ state }) => state === "infrastructure_failed" || state === "interrupted").slice(0, 8),
    },
    families: families.results.map((family) => ({
      id: family.id,
      task: family.task,
      treatment: {
        harness: family.harness,
        model: family.model,
        thinking: family.thinking,
        web_search: family.web_search !== 0,
      },
      desired: family.desired,
      ...summary(family),
    })),
  });
}

async function findLatestProfile(db: D1Database, profile: string): Promise<WorksetRow | null> {
  return db.prepare(
    `SELECT w.id, w.profile, w.digest, w.created_at_ms, COUNT(DISTINCT d.id) AS task_count, ${summaryColumns("e")} FROM worksets w LEFT JOIN task_definitions d ON d.workset_id = w.id LEFT JOIN eval_tasks e ON e.definition_id = d.id WHERE w.state = 'ready' AND w.profile = ?1 GROUP BY w.id ORDER BY w.created_at_ms DESC, w.id DESC LIMIT 1`,
  ).bind(profile).first<WorksetRow>();
}

async function readWorksets(db: D1Database): Promise<WorksetRow[]> {
  return (await db.prepare(
    `SELECT w.id, w.profile, w.digest, w.created_at_ms, COUNT(DISTINCT d.id) AS task_count, ${summaryColumns("e")} FROM worksets w LEFT JOIN task_definitions d ON d.workset_id = w.id LEFT JOIN eval_tasks e ON e.definition_id = d.id WHERE w.state = 'ready' GROUP BY w.id ORDER BY w.created_at_ms DESC`,
  ).all<WorksetRow>()).results;
}

async function findWorksetMetadata(
  db: D1Database,
  digest: string,
): Promise<{ id: number; task_count: number } | null> {
  return db.prepare(
    `SELECT w.id, COUNT(d.id) AS task_count
    FROM worksets w
    LEFT JOIN task_definitions d ON d.workset_id = w.id
    WHERE w.state = 'ready' AND w.digest = ?1
    GROUP BY w.id`,
  ).bind(digest).first<{ id: number; task_count: number }>();
}

async function findWorksetAndTask(db: D1Database, digest: string, taskId: string) {
  return db.prepare(
    "SELECT w.id AS worksetId, d.id AS taskId FROM worksets w JOIN task_definitions d ON d.workset_id = w.id WHERE w.state = 'ready' AND w.digest = ?1 AND d.public_id = ?2",
  ).bind(digest, taskId).first<{ worksetId: number; taskId: number }>();
}

async function readCoordinates(db: D1Database, worksetId: number, taskId?: number): Promise<CoordinateRow[]> {
  const clause = taskId == null ? "e.workset_id = ?1" : "e.workset_id = ?1 AND e.definition_id = ?2";
  const statement = db.prepare(
    `SELECT e.id, e.public_id, d.public_id AS task_public_id, d.name AS task_name, d.digest AS task_digest, e.family_key, e.harness, e.model, e.thinking, e.repetition, e.state, e.started_at_ms, e.finished_at_ms, e.error, r.case_key, r.status, r.outcome, r.input_tokens, r.cached_input_tokens, r.output_tokens, r.reasoning_output_tokens, r.total_tokens, r.cost_usd, r.agent_duration_ms FROM eval_tasks e JOIN task_definitions d ON d.id = e.definition_id LEFT JOIN coordinate_results r ON r.coordinate_id = e.id WHERE ${clause} ORDER BY e.id`,
  );
  return (await (taskId == null ? statement.bind(worksetId) : statement.bind(worksetId, taskId)).all<CoordinateRow>()).results;
}

function summaryColumns(alias: string): string {
  return `COUNT(${alias}.id) AS total, COALESCE(SUM(CASE WHEN ${alias}.state = 'unclaimed' THEN 1 ELSE 0 END), 0) AS unclaimed, COALESCE(SUM(CASE WHEN ${alias}.state = 'running' THEN 1 ELSE 0 END), 0) AS running, COALESCE(SUM(CASE WHEN ${alias}.state = 'success' THEN 1 ELSE 0 END), 0) AS success, COALESCE(SUM(CASE WHEN ${alias}.state = 'failed' THEN 1 ELSE 0 END), 0) AS failed`;
}

function publicWorkset(row: WorksetRow) {
  return {
    id: row.digest,
    profile: row.profile,
    digest: row.digest,
    createdAtMs: row.created_at_ms,
    taskCount: row.task_count,
    summary: summary(row),
  };
}

function summary(row: Summary): Summary {
  return {
    total: Number(row.total),
    unclaimed: Number(row.unclaimed),
    running: Number(row.running),
    success: Number(row.success),
    failed: Number(row.failed),
  };
}

function prefixedSummary(row: TaskSnapshotRow, prefix: "workset" | "task"): Summary {
  return {
    total: Number(prefix === "workset" ? row.workset_total : row.task_total),
    unclaimed: Number(prefix === "workset" ? row.workset_unclaimed : row.task_unclaimed),
    running: Number(prefix === "workset" ? row.workset_running : row.task_running),
    success: Number(prefix === "workset" ? row.workset_success : row.task_success),
    failed: Number(prefix === "workset" ? row.workset_failed : row.task_failed),
  };
}

function emptySummary(): Summary {
  return { total: 0, unclaimed: 0, running: 0, success: 0, failed: 0 };
}

function addSummary(total: Summary, row: Summary): Summary {
  total.total += Number(row.total);
  total.unclaimed += Number(row.unclaimed);
  total.running += Number(row.running);
  total.success += Number(row.success);
  total.failed += Number(row.failed);
  return total;
}

function duration(row: Pick<CoordinateRow, "agent_duration_ms">): number | null {
  return row.agent_duration_ms == null ? null : Math.max(0, row.agent_duration_ms);
}

function publicResultPoint(row: CoordinateRow) {
  return {
    id: row.public_id,
    taskId: row.task_public_id,
    taskName: row.task_name,
    taskLabel: shortName(row.task_name),
    state: row.state,
    harness: row.harness,
    model: row.model,
    thinking: row.thinking,
    repetition: row.repetition,
    status: row.status,
    outcome: row.outcome,
    durationMs: duration(row),
    inputTokens: row.input_tokens,
    cachedInputTokens: row.cached_input_tokens,
    outputTokens: row.output_tokens,
    reasoningOutputTokens: row.reasoning_output_tokens,
    totalTokens: row.total_tokens,
    costUsd: row.cost_usd,
  };
}

async function publicId(...parts: string[]): Promise<string> {
  const encoder = new TextEncoder();
  const bytes: number[] = [];
  for (const part of parts) {
    bytes.push(...encoder.encode(part), 0);
  }
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new Uint8Array(bytes)));
  return [...digest].map((byte) => byte.toString(16).padStart(2, "0")).join("").slice(0, 24);
}

function shortName(name: string): string {
  return name.split("/").at(-1) ?? name;
}

function boundedInteger(value: string | null, minimum: number, maximum: number, fallback: number): number {
  if (value == null || !/^\d+$/.test(value)) return fallback;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed >= minimum && parsed <= maximum ? parsed : fallback;
}

function json(body: unknown, status = 200): Response {
  return Response.json(body, { status, headers: JSON_HEADERS });
}
