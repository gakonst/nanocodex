const JSON_HEADERS = {
  "cache-control": "no-store",
  "content-type": "application/json; charset=utf-8",
  "x-content-type-options": "nosniff",
};
const CLAIM_LEASE_MS = 6 * 60 * 60 * 1_000;
const NODE_LEASE_MS = 60_000;
const MAX_SEED_TASKS = 10_000;
const MAX_ARTIFACT_BYTES = 100 * 1024 * 1024;
const ARCHIVE_CONTENT_TYPE = "application/x-tar+zstd";

export type EvalStorageEnv = {
  EVALS_DB?: D1Database;
  EVALS_ARTIFACTS?: R2Bucket;
  EVAL_COORDINATOR?: DurableObjectNamespace;
  EVALS_WRITE_TOKEN?: string;
};

type SeedCoordinate = {
  publicId: string;
  familyKey: string;
  harness: string;
  model: string;
  thinking: string;
  webSearch: boolean;
  repetition: number;
  state?: "unclaimed" | "success" | "failed";
  claimId?: string;
  worker?: string;
  startedAtMs?: number;
  finishedAtMs?: number;
  artifactKey?: string;
  error?: string;
  result?: SeedResult;
};

type SeedResult = {
  caseKey?: string;
  status?: string;
  outcome?: string;
  inputTokens?: number;
  cachedInputTokens?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  totalTokens?: number;
  costUsd?: number;
  durationMs?: number;
};

type SeedTask = {
  publicId: string;
  selector: string;
  name: string;
  digest: string;
  taskKey: string;
  coordinates: SeedCoordinate[];
};

type SeedWorkset = {
  profile: string;
  digest: string;
  createdAtMs: number;
  tasks: SeedTask[];
};

type ClaimRequest = {
  profile: string;
  task?: string;
  harness?: string;
  model?: string;
  thinking?: string;
  worker?: string;
};

type FinishRequest = {
  outcome: "success" | "failed" | "retry";
  error?: string;
  evidence?: string;
  case?: Record<string, unknown>;
};

type ClaimRow = {
  id: number;
  workset_id: number;
  workset_digest: string;
  public_id: string;
  selector: string;
  task_key: string;
  task_digest: string;
  family_key: string;
  harness: string;
  model: string;
  thinking: string;
  web_search: number;
  repetition: number;
};

type ActiveRow = ClaimRow & {
  claim_id: string;
  worker: string;
  started_at_ms: number;
  artifact_key: string | null;
};

type MultipartComplete = {
  parts: R2UploadedPart[];
};

export class EvalCoordinator {
  readonly #state: DurableObjectState;
  readonly #env: EvalStorageEnv;

  constructor(state: DurableObjectState, env: EvalStorageEnv) {
    this.#state = state;
    this.#env = env;
  }

  async fetch(request: Request): Promise<Response> {
    if (!this.#env.EVALS_DB || !this.#env.EVALS_ARTIFACTS) {
      return error("evaluation storage is not configured", 503);
    }
    const url = new URL(request.url);
    try {
      if (request.method === "PUT" && url.pathname === "/workset") {
        return this.#seedWorkset(await request.json());
      }
      if (request.method === "POST" && url.pathname === "/claims") {
        return this.#claim(await request.json());
      }
      const artifact = url.pathname.match(/^\/claims\/([^/]+)\/artifacts$/);
      if (request.method === "PUT" && artifact) {
        return this.#uploadArtifact(decodeURIComponent(artifact[1]), request);
      }
      const heartbeat = url.pathname.match(/^\/claims\/([^/]+)\/heartbeat$/);
      if (request.method === "POST" && heartbeat) {
        return this.#heartbeat(decodeURIComponent(heartbeat[1]));
      }
      const finish = url.pathname.match(/^\/claims\/([^/]+)\/finish$/);
      if (request.method === "POST" && finish) {
        return this.#finish(decodeURIComponent(finish[1]), await request.json());
      }
      if (request.method === "POST" && url.pathname === "/workers/exited") {
        return this.#workerExited(await request.json());
      }
      if (request.method === "PUT" && url.pathname === "/cluster/node") {
        return this.#updateClusterNode(await request.json());
      }
      if (["HEAD", "PUT", "DELETE"].includes(request.method) && url.pathname === "/import/object") {
        return this.#importObject(request, url);
      }
      if (request.method === "POST" && url.pathname === "/import/multipart") {
        return this.#createMultipart(url, request);
      }
      const multipartPart = url.pathname.match(/^\/import\/multipart\/([^/]+)\/parts\/(\d+)$/);
      if (request.method === "PUT" && multipartPart) {
        return this.#uploadMultipartPart(
          url,
          decodeURIComponent(multipartPart[1]),
          Number(multipartPart[2]),
          request,
        );
      }
      const multipart = url.pathname.match(/^\/import\/multipart\/([^/]+)$/);
      if (multipart && request.method === "POST") {
        return this.#completeMultipart(
          url,
          decodeURIComponent(multipart[1]),
          await request.json(),
        );
      }
      if (multipart && request.method === "DELETE") {
        return this.#abortMultipart(url, decodeURIComponent(multipart[1]));
      }
      return error("not_found", 404);
    } catch (cause) {
      console.error({
        type: "evaluation.coordinator_mutation_failed",
        error_kind: cause instanceof Error ? cause.name : typeof cause,
      });
      return error("evaluation coordinator mutation failed", 500);
    }
  }

  async alarm(): Promise<void> {
    await this.#releaseExpiredClaims();
  }

  async #seedWorkset(input: unknown): Promise<Response> {
    if (!isSeedWorkset(input)) return error("invalid workset", 400);
    const db = this.#env.EVALS_DB!;
    const existing = await db.prepare(
      "SELECT id FROM worksets WHERE digest = ?1",
    ).bind(input.digest).first<{ id: number }>();
    let worksetId = existing?.id;
    if (worksetId == null) {
      const inserted = await db.prepare(
        "INSERT INTO worksets(profile, digest, created_at_ms, state) VALUES (?1, ?2, ?3, 'materializing') RETURNING id",
      ).bind(input.profile, input.digest, input.createdAtMs).first<{ id: number }>();
      worksetId = inserted?.id;
    } else {
      const running = await db.prepare(
        "SELECT COUNT(*) AS count FROM eval_tasks WHERE workset_id = ?1 AND state = 'running'",
      ).bind(worksetId).first<{ count: number }>();
      if ((running?.count ?? 0) > 0) return error("workset has active claims", 409);
      await db.batch([
        db.prepare("UPDATE worksets SET profile = ?1, created_at_ms = ?2, state = 'materializing' WHERE id = ?3")
          .bind(input.profile, input.createdAtMs, worksetId),
        db.prepare("DELETE FROM task_definitions WHERE workset_id = ?1").bind(worksetId),
      ]);
    }
    if (worksetId == null) return error("workset insert failed", 500);

    for (const task of input.tasks) {
      const definition = await db.prepare(
        "INSERT INTO task_definitions(workset_id, public_id, selector, name, digest, task_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6) RETURNING id",
      ).bind(
        worksetId,
        task.publicId,
        task.selector,
        task.name,
        task.digest,
        task.taskKey,
      ).first<{ id: number }>();
      if (definition == null) return error("task definition insert failed", 500);
      for (let start = 0; start < task.coordinates.length; start += 75) {
        await db.batch(task.coordinates.slice(start, start + 75).map((coordinate) =>
          db.prepare(
            "INSERT INTO eval_tasks(public_id, workset_id, definition_id, family_key, harness, model, thinking, web_search, repetition, state, claim_id, worker, started_at_ms, finished_at_ms, artifact_key, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
          ).bind(
            coordinate.publicId,
            worksetId,
            definition.id,
            coordinate.familyKey,
            coordinate.harness,
            coordinate.model,
            coordinate.thinking,
            coordinate.webSearch ? 1 : 0,
            coordinate.repetition,
            coordinate.state ?? "unclaimed",
            importedClaimId(input, coordinate),
            coordinate.state && coordinate.state !== "unclaimed"
              ? coordinate.worker ?? "historical-import"
              : null,
            importedStartedAt(input, coordinate),
            importedFinishedAt(input, coordinate),
            coordinate.artifactKey ?? null,
            coordinate.error ?? null,
          )
        ));
      }
      const insertedCoordinates = await db.prepare(
        "SELECT id, public_id FROM eval_tasks WHERE definition_id = ?1",
      ).bind(definition.id).all<{ id: number; public_id: string }>();
      const ids = new Map(insertedCoordinates.results.map((row) => [row.public_id, row.id]));
      const historical = task.coordinates.filter((coordinate) =>
        coordinate.state === "success" || coordinate.state === "failed"
      );
      const statements = historical.flatMap((coordinate) => {
        const coordinateId = ids.get(coordinate.publicId);
        if (coordinateId == null) throw new Error("imported coordinate insert was not visible");
        const claimId = importedClaimId(input, coordinate)!;
        const result = coordinate.result;
        return [
          db.prepare(
            "INSERT INTO eval_attempts(workset_id, task_id, claim_id, worker, state, started_at_ms, finished_at_ms, artifact_key, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
          ).bind(
            worksetId,
            coordinateId,
            claimId,
            coordinate.worker ?? "historical-import",
            coordinate.state === "success" ? "passed" : "failed",
            importedStartedAt(input, coordinate),
            importedFinishedAt(input, coordinate),
            coordinate.artifactKey ?? null,
            coordinate.error ?? null,
          ),
          db.prepare(
            "INSERT INTO coordinate_results(coordinate_id, case_key, status, outcome, input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens, total_tokens, cost_usd, agent_duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
          ).bind(
            coordinateId,
            result?.caseKey ?? null,
            result?.status ?? null,
            result?.outcome ?? null,
            result?.inputTokens ?? null,
            result?.cachedInputTokens ?? null,
            result?.outputTokens ?? null,
            result?.reasoningOutputTokens ?? null,
            result?.totalTokens ?? null,
            result?.costUsd ?? estimatedCostUsd(coordinate.model, result),
            result?.durationMs ?? null,
          ),
        ];
      });
      for (let start = 0; start < statements.length; start += 80) {
        await db.batch(statements.slice(start, start + 80));
      }
    }
    await db.prepare("UPDATE worksets SET state = 'ready' WHERE id = ?1")
      .bind(worksetId).run();
    return Response.json({ digest: input.digest, tasks: input.tasks.length }, { headers: JSON_HEADERS });
  }

  async #claim(input: unknown): Promise<Response> {
    if (!isClaimRequest(input)) return error("invalid claim request", 400);
    await this.#releaseExpiredClaims();
    const db = this.#env.EVALS_DB!;
    const workset = await db.prepare(
      "SELECT id FROM worksets WHERE profile = ?1 AND state = 'ready' ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    ).bind(input.profile).first<{ id: number }>();
    if (workset == null) return error("evaluation profile was not found", 404);
    const filters = [
      "w.id = ?1",
      "e.state = 'unclaimed'",
    ];
    const values: unknown[] = [workset.id];
    for (const [column, value] of [
      ["d.selector", input.task],
      ["e.harness", input.harness],
      ["e.model", input.model],
      ["e.thinking", input.thinking],
    ] as const) {
      if (value == null) continue;
      values.push(value);
      filters.push(`${column} = ?${values.length}`);
    }
    const selection = claimSelectionSql(filters.join(" AND "));
    const row = await db.prepare(selection).bind(...values).first<ClaimRow>();
    if (row == null) {
      const activeFilters = filters.map((filter) =>
        filter === "e.state = 'unclaimed'" ? "e.state = 'running'" : filter
      );
      const active = await db.prepare(
        `SELECT COUNT(*) AS count FROM eval_tasks e JOIN task_definitions d ON d.id = e.definition_id JOIN worksets w ON w.id = e.workset_id WHERE ${activeFilters.join(" AND ")}`,
      ).bind(...values).first<{ count: number }>();
      return Response.json(
        (active?.count ?? 0) > 0
          ? { action: "busy", reason: "matching coordinates are currently running", retry_after_ms: 1_000 }
          : { action: "complete" },
        { headers: JSON_HEADERS },
      );
    }

    const claimId = crypto.randomUUID();
    const worker = input.worker?.trim() || "anonymous-worker";
    const startedAt = Date.now();
    const leaseExpiresAt = startedAt + CLAIM_LEASE_MS;
    const results = await db.batch([
      db.prepare(
        "UPDATE eval_tasks SET state = 'running', claim_id = ?1, worker = ?2, started_at_ms = ?3, lease_expires_at_ms = ?4 WHERE id = ?5 AND state = 'unclaimed'",
      ).bind(claimId, worker, startedAt, leaseExpiresAt, row.id),
      db.prepare(
        "INSERT INTO eval_attempts(workset_id, task_id, claim_id, worker, state, started_at_ms) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
      ).bind(row.workset_id, row.id, claimId, worker, startedAt),
    ]);
    if (results[0].meta.changes !== 1) return error("claim conflict", 409);
    await this.#state.storage.setAlarm(leaseExpiresAt);
    return Response.json({
      action: "run",
      claim: claimId,
      repetition: row.repetition,
      family_key: row.family_key,
      task: row.selector,
      task_package: row.task_key,
      task_digest: row.task_digest,
      treatment: {
        harness: row.harness,
        model: row.model,
        thinking: row.thinking,
        web_search: row.web_search !== 0,
      },
    }, { headers: JSON_HEADERS });
  }

  async #uploadArtifact(claimId: string, request: Request): Promise<Response> {
    if (request.headers.get("content-type") !== ARCHIVE_CONTENT_TYPE) {
      return error(`artifacts require ${ARCHIVE_CONTENT_TYPE}`, 415);
    }
    const length = Number(request.headers.get("content-length"));
    if (Number.isFinite(length) && length > MAX_ARTIFACT_BYTES) {
      return error("artifact upload is too large", 413);
    }
    const active = await this.#activeClaim(claimId);
    if (active == null) return error("claim is absent or expired", 404);
    if (request.body == null) return error("artifact upload is empty", 400);
    const key = `attempts/${active.workset_digest}/${claimId}/evidence.tar.zst`;
    await this.#env.EVALS_ARTIFACTS!.put(key, request.body, {
      httpMetadata: { contentType: ARCHIVE_CONTENT_TYPE },
      customMetadata: {
        claim: claimId,
        coordinate: active.public_id,
        worker: active.worker,
      },
    });
    await this.#env.EVALS_DB!.batch([
      this.#env.EVALS_DB!.prepare(
        "UPDATE eval_tasks SET artifact_key = ?1 WHERE id = ?2 AND claim_id = ?3 AND state = 'running'",
      ).bind(key, active.id, claimId),
      this.#env.EVALS_DB!.prepare(
        "UPDATE eval_attempts SET artifact_key = ?1 WHERE claim_id = ?2 AND state = 'running'",
      ).bind(key, claimId),
    ]);
    return new Response(null, { status: 204 });
  }

  async #heartbeat(claimId: string): Promise<Response> {
    const expiresAt = Date.now() + CLAIM_LEASE_MS;
    const result = await this.#env.EVALS_DB!.prepare(
      "UPDATE eval_tasks SET lease_expires_at_ms = ?1 WHERE claim_id = ?2 AND state = 'running'",
    ).bind(expiresAt, claimId).run();
    if (result.meta.changes !== 1) return error("claim is absent or expired", 404);
    await this.#state.storage.setAlarm(expiresAt);
    return new Response(null, { status: 204 });
  }

  async #finish(claimId: string, input: unknown): Promise<Response> {
    if (!isFinishRequest(input)) return error("invalid finish request", 400);
    const db = this.#env.EVALS_DB!;
    const active = await this.#activeClaim(claimId);
    if (active == null) return error("claim is absent or expired", 404);
    if (input.evidence && !active.artifact_key) {
      return error("accepted evidence was not uploaded", 400);
    }
    const finishedAt = Date.now();
    if (input.outcome === "retry") {
      const results = await db.batch([
        db.prepare(
          "UPDATE eval_tasks SET state = 'unclaimed', claim_id = NULL, worker = NULL, started_at_ms = NULL, finished_at_ms = NULL, lease_expires_at_ms = NULL, artifact_key = NULL, error = NULL WHERE id = ?1 AND claim_id = ?2 AND state = 'running'",
        ).bind(active.id, claimId),
        db.prepare(
          "UPDATE eval_attempts SET state = 'infrastructure_failed', finished_at_ms = ?1, error = ?2 WHERE claim_id = ?3 AND state = 'running'",
        ).bind(finishedAt, input.error ?? "retryable failure", claimId),
      ]);
      if (results[0].meta.changes !== 1) return error("stale claim", 409);
      return new Response(null, { status: 204 });
    }

    const coordinateState = input.outcome === "success" ? "success" : "failed";
    const attemptState = input.outcome === "success" ? "passed" : "failed";
    let caseKey: string | null = null;
    const metrics = caseMetrics(input.case);
    if (input.case) {
      caseKey = `cases/${active.workset_digest}/${active.public_id}.json`;
      await this.#env.EVALS_ARTIFACTS!.put(caseKey, JSON.stringify(input.case), {
        httpMetadata: { contentType: "application/json" },
      });
    }
    const results = await db.batch([
      db.prepare(
        "UPDATE eval_tasks SET state = ?1, finished_at_ms = ?2, lease_expires_at_ms = NULL, error = ?3 WHERE id = ?4 AND claim_id = ?5 AND state = 'running'",
      ).bind(coordinateState, finishedAt, input.error ?? null, active.id, claimId),
      db.prepare(
        "UPDATE eval_attempts SET state = ?1, finished_at_ms = ?2, error = ?3 WHERE claim_id = ?4 AND state = 'running'",
      ).bind(attemptState, finishedAt, input.error ?? null, claimId),
      db.prepare(
        "INSERT INTO coordinate_results(coordinate_id, case_key, status, outcome, input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens, total_tokens, cost_usd, agent_duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT(coordinate_id) DO UPDATE SET case_key = excluded.case_key, status = excluded.status, outcome = excluded.outcome, input_tokens = excluded.input_tokens, cached_input_tokens = excluded.cached_input_tokens, output_tokens = excluded.output_tokens, reasoning_output_tokens = excluded.reasoning_output_tokens, total_tokens = excluded.total_tokens, cost_usd = excluded.cost_usd, agent_duration_ms = excluded.agent_duration_ms",
      ).bind(
        active.id,
        caseKey,
        metrics.status,
        metrics.outcome,
        metrics.inputTokens,
        metrics.cachedInputTokens,
        metrics.outputTokens,
        metrics.reasoningOutputTokens,
        metrics.totalTokens,
        metrics.costUsd ?? estimatedCostUsd(active.model, metrics),
        metrics.durationMs,
      ),
    ]);
    if (results[0].meta.changes !== 1) return error("stale claim", 409);
    return new Response(null, { status: 204 });
  }

  async #workerExited(input: unknown): Promise<Response> {
    const body = asRecord(input);
    const worker = typeof body?.worker === "string" || typeof body?.worker === "number"
      ? String(body.worker).trim()
      : "";
    const message = typeof body?.error === "string" ? body.error.trim() : "";
    if (!worker || !message) return error("worker and error are required", 400);
    const db = this.#env.EVALS_DB!;
    const claims = await db.prepare(
      "SELECT claim_id FROM eval_tasks WHERE worker = ?1 AND state = 'running'",
    ).bind(worker).all<{ claim_id: string }>();
    if (!claims.results.length) return new Response(null, { status: 204 });
    const finishedAt = Date.now();
    await db.batch([
      db.prepare(
        "UPDATE eval_tasks SET state = 'unclaimed', claim_id = NULL, worker = NULL, started_at_ms = NULL, finished_at_ms = NULL, lease_expires_at_ms = NULL, artifact_key = NULL, error = NULL WHERE worker = ?1 AND state = 'running'",
      ).bind(worker),
      ...claims.results.map(({ claim_id }) => db.prepare(
        "UPDATE eval_attempts SET state = 'interrupted', finished_at_ms = ?1, error = ?2 WHERE claim_id = ?3 AND state = 'running'",
      ).bind(finishedAt, message, claim_id)),
    ]);
    return new Response(null, { status: 204 });
  }

  async #updateClusterNode(input: unknown): Promise<Response> {
    const body = asRecord(input);
    if (!body || typeof body.id !== "string" || !body.id.trim()) {
      return error("cluster node id is required", 400);
    }
    const observedAt = Date.now();
    await this.#env.EVALS_DB!.prepare(
      "INSERT INTO cluster_nodes(id, observed_at_ms, expires_at_ms, payload_json) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(id) DO UPDATE SET observed_at_ms = excluded.observed_at_ms, expires_at_ms = excluded.expires_at_ms, payload_json = excluded.payload_json",
    ).bind(body.id, observedAt, observedAt + NODE_LEASE_MS, JSON.stringify(body)).run();
    return new Response(null, { status: 204 });
  }

  async #importObject(request: Request, url: URL): Promise<Response> {
    const key = importKey(url);
    if (key == null) return error("invalid import object key", 400);
    const bucket = this.#env.EVALS_ARTIFACTS!;
    if (request.method === "HEAD") {
      const object = await bucket.head(key);
      return object == null
        ? new Response(null, { status: 404 })
        : new Response(null, {
          status: 204,
          headers: {
            etag: object.httpEtag,
            "x-r2-size": String(object.size),
          },
        });
    }
    if (request.method === "DELETE") {
      await bucket.delete(key);
      return new Response(null, { status: 204 });
    }
    if (request.body == null) return error("import object is empty", 400);
    const length = Number(request.headers.get("content-length"));
    if (Number.isFinite(length) && length > MAX_ARTIFACT_BYTES) {
      return error("use multipart upload for objects larger than 100 MiB", 413);
    }
    const object = await bucket.put(key, request.body, {
      httpMetadata: {
        contentType: request.headers.get("content-type") ?? "application/octet-stream",
      },
    });
    return Response.json(
      { key: object.key, size: object.size, etag: object.etag },
      { status: 201, headers: JSON_HEADERS },
    );
  }

  async #createMultipart(url: URL, request: Request): Promise<Response> {
    const key = importKey(url);
    if (key == null) return error("invalid import object key", 400);
    const upload = await this.#env.EVALS_ARTIFACTS!.createMultipartUpload(key, {
      httpMetadata: {
        contentType: request.headers.get("content-type") ?? "application/octet-stream",
      },
    });
    return Response.json(
      { key: upload.key, uploadId: upload.uploadId },
      { status: 201, headers: JSON_HEADERS },
    );
  }

  async #uploadMultipartPart(
    url: URL,
    uploadId: string,
    partNumber: number,
    request: Request,
  ): Promise<Response> {
    const key = importKey(url);
    if (key == null || !Number.isInteger(partNumber) || partNumber < 1 || partNumber > 10_000) {
      return error("invalid multipart part", 400);
    }
    if (!uploadId || request.body == null) return error("multipart part is empty", 400);
    const upload = this.#env.EVALS_ARTIFACTS!.resumeMultipartUpload(key, uploadId);
    const part = await upload.uploadPart(partNumber, request.body);
    return Response.json(part, { headers: JSON_HEADERS });
  }

  async #completeMultipart(url: URL, uploadId: string, input: unknown): Promise<Response> {
    const key = importKey(url);
    if (key == null || !uploadId || !isMultipartComplete(input)) {
      return error("invalid multipart completion", 400);
    }
    const upload = this.#env.EVALS_ARTIFACTS!.resumeMultipartUpload(key, uploadId);
    const object = await upload.complete(input.parts);
    return Response.json(
      { key: object.key, size: object.size, etag: object.etag },
      { headers: JSON_HEADERS },
    );
  }

  async #abortMultipart(url: URL, uploadId: string): Promise<Response> {
    const key = importKey(url);
    if (key == null || !uploadId) return error("invalid multipart upload", 400);
    await this.#env.EVALS_ARTIFACTS!.resumeMultipartUpload(key, uploadId).abort();
    return new Response(null, { status: 204 });
  }

  async #activeClaim(claimId: string): Promise<ActiveRow | null> {
    return this.#env.EVALS_DB!.prepare(
      "SELECT e.id, e.workset_id, w.digest AS workset_digest, e.public_id, d.selector, d.task_key, d.digest AS task_digest, e.family_key, e.harness, e.model, e.thinking, e.web_search, e.repetition, e.claim_id, e.worker, e.started_at_ms, e.artifact_key FROM eval_tasks e JOIN task_definitions d ON d.id = e.definition_id JOIN worksets w ON w.id = e.workset_id WHERE e.claim_id = ?1 AND e.state = 'running' LIMIT 1",
    ).bind(claimId).first<ActiveRow>();
  }

  async #releaseExpiredClaims(): Promise<void> {
    const db = this.#env.EVALS_DB!;
    const now = Date.now();
    const expired = await db.prepare(
      "SELECT claim_id FROM eval_tasks WHERE state = 'running' AND lease_expires_at_ms <= ?1",
    ).bind(now).all<{ claim_id: string }>();
    if (expired.results.length) {
      await db.batch([
        db.prepare(
          "UPDATE eval_tasks SET state = 'unclaimed', claim_id = NULL, worker = NULL, started_at_ms = NULL, finished_at_ms = NULL, lease_expires_at_ms = NULL, artifact_key = NULL, error = NULL WHERE state = 'running' AND lease_expires_at_ms <= ?1",
        ).bind(now),
        ...expired.results.map(({ claim_id }) => db.prepare(
          "UPDATE eval_attempts SET state = 'interrupted', finished_at_ms = ?1, error = 'claim lease expired' WHERE claim_id = ?2 AND state = 'running'",
        ).bind(now, claim_id)),
      ]);
    }
    const next = await db.prepare(
      "SELECT MIN(lease_expires_at_ms) AS expires_at FROM eval_tasks WHERE state = 'running'",
    ).first<{ expires_at: number | null }>();
    if (next?.expires_at != null) await this.#state.storage.setAlarm(next.expires_at);
  }
}

export async function routeEvalMutation(
  request: Request,
  env: EvalStorageEnv,
  url: URL,
): Promise<Response | null> {
  if (!isMutationPath(url.pathname, request.method)) return null;
  if (!env.EVALS_WRITE_TOKEN || !env.EVAL_COORDINATOR) {
    return error("evaluation coordinator is not configured", 503);
  }
  if (!authorized(request, env.EVALS_WRITE_TOKEN)) return error("unauthorized", 401);
  const path = mutationObjectPath(url.pathname);
  if (path == null) return error("not_found", 404);
  const id = env.EVAL_COORDINATOR.idFromName("global");
  const headers = new Headers(request.headers);
  headers.delete("authorization");
  const init: RequestInit & { duplex?: "half" } = {
    method: request.method,
    headers,
    body: request.body,
  };
  if (request.body != null) init.duplex = "half";
  return env.EVAL_COORDINATOR.get(id).fetch(
    new Request(`https://eval-coordinator${path}${url.search}`, init),
  );
}

export function authorized(request: Request, expected: string): boolean {
  const value = request.headers.get("authorization");
  if (!value?.startsWith("Bearer ")) return false;
  const actual = value.slice("Bearer ".length);
  if (actual.length !== expected.length) return false;
  let mismatch = 0;
  for (let index = 0; index < actual.length; index++) {
    mismatch |= actual.charCodeAt(index) ^ expected.charCodeAt(index);
  }
  return mismatch === 0;
}

function isMutationPath(path: string, method: string): boolean {
  return method !== "GET" && (
    path === "/v1/worksets" ||
    path === "/v1/claims" ||
    path === "/v1/workers/exited" ||
    path === "/v1/cluster/nodes" ||
    path === "/v1/import/objects" ||
    path === "/v1/import/multipart" ||
    /^\/v1\/import\/multipart\/[^/]+(?:\/parts\/\d+)?$/.test(path) ||
    /^\/v1\/claims\/[^/]+\/(artifacts|heartbeat|finish)$/.test(path)
  );
}

function mutationObjectPath(path: string): string | null {
  if (path === "/v1/worksets") return "/workset";
  if (path === "/v1/claims") return "/claims";
  if (path === "/v1/workers/exited") return "/workers/exited";
  if (path === "/v1/cluster/nodes") return "/cluster/node";
  if (path === "/v1/import/objects") return "/import/object";
  if (path === "/v1/import/multipart") return "/import/multipart";
  const multipart = path.match(/^\/v1\/import\/multipart\/([^/]+)(\/parts\/\d+)?$/);
  if (multipart) return `/import/multipart/${multipart[1]}${multipart[2] ?? ""}`;
  const claim = path.match(/^\/v1\/claims\/([^/]+)\/(artifacts|heartbeat|finish)$/);
  return claim ? `/claims/${claim[1]}/${claim[2]}` : null;
}

function claimSelectionSql(where: string): string {
  return `SELECT e.id, e.workset_id, w.digest AS workset_digest, e.public_id, d.selector, d.task_key, d.digest AS task_digest, e.family_key, e.harness, e.model, e.thinking, e.web_search, e.repetition FROM eval_tasks e JOIN task_definitions d ON d.id = e.definition_id JOIN worksets w ON w.id = e.workset_id WHERE ${where} ORDER BY e.id LIMIT 1`;
}

function isSeedWorkset(value: unknown): value is SeedWorkset {
  const body = asRecord(value);
  if (!body || !shortString(body.profile) || !shortString(body.digest) ||
    !Number.isSafeInteger(body.createdAtMs) || !Array.isArray(body.tasks) ||
    body.tasks.length > MAX_SEED_TASKS) return false;
  return body.tasks.every((task) => {
    const row = asRecord(task);
    return row && shortString(row.publicId) && shortString(row.selector) &&
      shortString(row.name) && shortString(row.digest) &&
      validImportKey(row.taskKey) && String(row.taskKey).startsWith("tasks/") &&
      Array.isArray(row.coordinates) && row.coordinates.every((coordinate) => {
        const cell = asRecord(coordinate);
        return cell && shortString(cell.publicId) && shortString(cell.familyKey) &&
          typeof cell.harness === "string" && typeof cell.model === "string" &&
          typeof cell.thinking === "string" && typeof cell.webSearch === "boolean" &&
          Number.isInteger(cell.repetition) && Number(cell.repetition) >= 0 &&
          Number(cell.repetition) <= 65_535 && validSeedState(cell);
      });
  });
}

function validSeedState(cell: Record<string, unknown>): boolean {
  const state = cell.state ?? "unclaimed";
  if (!["unclaimed", "success", "failed"].includes(String(state))) return false;
  if (state === "unclaimed") {
    return ["claimId", "worker", "startedAtMs", "finishedAtMs", "artifactKey", "error", "result"]
      .every((key) => cell[key] == null);
  }
  return (cell.claimId == null || shortString(cell.claimId)) &&
    (cell.worker == null || shortString(cell.worker)) &&
    optionalSafeInteger(cell.startedAtMs) && optionalSafeInteger(cell.finishedAtMs) &&
    (cell.artifactKey == null || validImportKey(cell.artifactKey)) &&
    (cell.error == null || typeof cell.error === "string") &&
    validSeedResult(cell.result);
}

function validSeedResult(value: unknown): boolean {
  if (value == null) return true;
  const result = asRecord(value);
  return result != null &&
    (result.caseKey == null || validImportKey(result.caseKey)) &&
    ["status", "outcome"].every((key) => result[key] == null || typeof result[key] === "string") &&
    ["inputTokens", "cachedInputTokens", "outputTokens", "reasoningOutputTokens", "totalTokens"]
      .every((key) => optionalSafeInteger(result[key])) &&
    optionalSafeInteger(result.durationMs) &&
    (result.costUsd == null || (typeof result.costUsd === "number" && Number.isFinite(result.costUsd)));
}

function importedClaimId(workset: SeedWorkset, coordinate: SeedCoordinate): string | null {
  if (!coordinate.state || coordinate.state === "unclaimed") return null;
  return coordinate.claimId ?? `import:${workset.digest}:${coordinate.publicId}`;
}

function importedStartedAt(workset: SeedWorkset, coordinate: SeedCoordinate): number | null {
  if (!coordinate.state || coordinate.state === "unclaimed") return null;
  return coordinate.startedAtMs ?? workset.createdAtMs;
}

function importedFinishedAt(workset: SeedWorkset, coordinate: SeedCoordinate): number | null {
  if (!coordinate.state || coordinate.state === "unclaimed") return null;
  return coordinate.finishedAtMs ?? coordinate.startedAtMs ?? workset.createdAtMs;
}

function importKey(url: URL): string | null {
  const key = url.searchParams.get("key");
  return key != null && validImportKey(key) ? key : null;
}

export function validImportKey(value: unknown): value is string {
  if (typeof value !== "string" || value.length < 1 || value.length > 1_024 ||
    value.includes("\\") || value.includes("//")) return false;
  const segments = value.split("/");
  return ["attempts", "cases", "tasks"].includes(segments[0]) &&
    segments.every((segment) => segment.length > 0 && segment !== "." && segment !== "..");
}

function isMultipartComplete(value: unknown): value is MultipartComplete {
  const body = asRecord(value);
  return body != null && Array.isArray(body.parts) && body.parts.length > 0 &&
    body.parts.length <= 10_000 && body.parts.every((value) => {
      const part = asRecord(value);
      return part != null && Number.isInteger(part.partNumber) && Number(part.partNumber) >= 1 &&
        Number(part.partNumber) <= 10_000 && shortString(part.etag);
    });
}

function optionalSafeInteger(value: unknown): boolean {
  return value == null || Number.isSafeInteger(value);
}

function isClaimRequest(value: unknown): value is ClaimRequest {
  const body = asRecord(value);
  return body != null && shortString(body.profile) && ["task", "harness", "model", "thinking", "worker"]
    .every((key) => body[key] == null || shortString(body[key]));
}

function isFinishRequest(value: unknown): value is FinishRequest {
  const body = asRecord(value);
  return body != null && ["success", "failed", "retry"].includes(String(body.outcome)) &&
    (body.error == null || typeof body.error === "string") &&
    (body.evidence == null || typeof body.evidence === "string") &&
    (body.case == null || asRecord(body.case) != null);
}

function caseMetrics(value: Record<string, unknown> | undefined) {
  const agent = asRecord(value?.agent);
  const usage = asRecord(value?.usage) ?? asRecord(agent?.usage);
  const estimatedCost = asRecord(asRecord(agent?.metadata)?.estimated_cost);
  const execution = asRecord(asRecord(value?.timing)?.agent_execution);
  return {
    status: stringOrNull(value?.status),
    outcome: stringOrNull(value?.outcome),
    inputTokens: integerOrNull(usage?.input_tokens),
    cachedInputTokens: integerOrNull(usage?.cached_input_tokens),
    outputTokens: integerOrNull(usage?.output_tokens),
    reasoningOutputTokens: integerOrNull(usage?.reasoning_output_tokens),
    totalTokens: integerOrNull(usage?.total_tokens),
    costUsd: numberOrNull(value?.costUsd) ?? numberOrNull(agent?.cost_usd) ?? numberOrNull(estimatedCost?.usd),
    durationMs: phaseDurationMs(execution),
  };
}

export function estimatedCostUsd(
  model: string,
  usage: {
    inputTokens?: number | null;
    cachedInputTokens?: number | null;
    outputTokens?: number | null;
  } | undefined,
): number | null {
  const rates = model === "sol" || model === "gpt-5.6-sol"
    ? { input: 4, cached: 0.4, output: 20 }
    : model === "terra" || model === "gpt-5.6-terra"
      ? { input: 2, cached: 0.2, output: 12 }
      : model === "luna" || model === "gpt-5.6-luna"
        ? { input: 0.2, cached: 0.02, output: 1.2 }
        : model === "astra" || model === "gpt-6-astra"
          ? { input: 10, cached: 1, output: 50 }
        : null;
  const input = usage?.inputTokens;
  const output = usage?.outputTokens;
  if (rates == null || input == null || output == null) return null;
  const cached = Math.max(0, Math.min(input, usage?.cachedInputTokens ?? 0));
  const ordinary = Math.max(0, input - cached);
  return (ordinary * rates.input + cached * rates.cached + output * rates.output) / 1_000_000;
}

function phaseDurationMs(phase: Record<string, unknown> | null): number | null {
  const started = timestampNanoseconds(phase?.started_at);
  const finished = timestampNanoseconds(phase?.finished_at);
  if (started == null || finished == null || finished < started) return null;
  return Number((finished - started) / 1_000_000n);
}

function timestampNanoseconds(value: unknown): bigint | null {
  if (typeof value !== "string") return null;
  const match = /^(.*?)(?:\.(\d{1,9}))?Z$/.exec(value);
  if (!match) return null;
  const milliseconds = Date.parse(`${match[1]}Z`);
  if (!Number.isFinite(milliseconds)) return null;
  const fractional = BigInt((match[2] ?? "").padEnd(9, "0"));
  return BigInt(milliseconds) * 1_000_000n + fractional;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value != null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function shortString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= 1_024;
}

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function integerOrNull(value: unknown): number | null {
  if (typeof value === "number" && Number.isSafeInteger(value)) return value;
  if (typeof value === "string" && /^\d+$/.test(value)) {
    const parsed = Number(value);
    return Number.isSafeInteger(parsed) ? parsed : null;
  }
  return null;
}

function numberOrNull(value: unknown): number | null {
  const parsed = typeof value === "number" ? value
    : typeof value === "string" ? Number(value) : NaN;
  return Number.isFinite(parsed) ? parsed : null;
}

function error(message: string, status: number): Response {
  return Response.json({ error: message }, { status, headers: JSON_HEADERS });
}
