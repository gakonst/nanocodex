import type { NamedTool, ToolContext } from "nanocodex";
import { stageFunctionCallOutput } from "../../nanocodex/host/internal-Agent.mjs";

/** Durable background jobs for explicitly registered owner-scoped tools. */
// Unreal Agent's MIT-licensed context-builder pending output; source:
// https://github.com/unreallabsai/unreal-agent/blob/1b9f778453f411c029b39b85102aaefb95e7e48d/harness/contextbuilder/builder.go
export const UNREAL_RUNNING_OUTPUT = "Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it.";
const MAX_INPUT = 64_000;
const MAX_STATUS_RESULT = 8_192;
const MAX_DURABLE_RESULT = 1_048_576;
const LEASE_MS = 30_000;
const MAX_ATTEMPTS = 3;
const MAX_JOBS = 100;
const MAX_ACTIVE = 8;
// Sample SQL databaseSize, which includes native journals and host tombstones.
// This is a protective admission throttle, not a hard size bound: native
// writes can grow between samples and the reserve is not an enforced maximum.
const MAX_ASYNC_DATABASE_BYTES = 256 * 1024 * 1024;
const ASYNC_ADMISSION_HEADROOM_BYTES = 64 * 1024 * 1024;
const RETENTION_MS = 7 * 24 * 60 * 60 * 1000;
// Once the originating turn settles, replay the identical stable operation
// once at an idle boundary. An active-turn acceptance may be discarded by
// cancellation before a model request receives it. This generation records
// the post-settlement attempt; it does not assert model uptake.
const WAKE_GENERATION = 1;
const bound = (value: string) => value.length > MAX_STATUS_RESULT ? `${value.slice(0, MAX_STATUS_RESULT)}\n[truncated in status; original output retained]` : value;

type Job = { id: string; invocation: string; original_turn: string; execution_turn: string | null; call_id: string | null;
  tool: string; args: string; state: string; result: string | null; attempts: number; started_at: number | null; created_at: number;
  terminal_state: string | null; delivered_at: number | null; continuation_started: number | null; wake_generation: number; lease_id: string | null; context_json: string | null; replay_safe: number; };
/** Durable intent, NOT a provider output. ToolContext.turnId identifies a JS
 * execution; it must not be assumed to identify a Rust Agent turn. */
export type FinalToolResultIntent = Readonly<{ originalTurn: string; executionTurn: string; callId: string;
  tool: string; jobId: string; terminalState: "completed" | "failed" | "uncertain" | "cancelled"; output: string }>;
/** A trusted adapter must resolve the original Agent tool call, atomically
 * replace its unsent pending output OR append the terminal output under the
 * same call ID if pending was already sent, and durably dedupe jobId.
 * Never implement this with turn.prompt(). */
export type FinalToolResultReceipt = Readonly<{ operation_id: string; call_id: string;
  replayed: boolean; continuation_started: boolean }>;
// The host bridge returns an unverified JSON object; only a checked receipt
// may advance the durable job, regardless of its permissive TypeScript type.
export type DeliverFinalToolResult = (intent: FinalToolResultIntent) => Promise<unknown>;
export type DeliverFinalToolResults = (intents: readonly FinalToolResultIntent[]) => Promise<unknown>;
export type OutputStatus = (intent: FinalToolResultIntent) => Promise<unknown>;
export class TypedIngestionUnavailable extends Error {
  constructor() { super("typed same-call-ID result ingestion is not available"); }
}

const terminalOrigin = `EXISTS (SELECT 1 FROM turns AS source_turn
  WHERE source_turn.id = async_jobs.original_turn
    AND source_turn.state IN ('completed', 'failed', 'cancelled'))`;

export class AsyncJobs {
  private readonly active = new Map<string, string>();
  private readonly delivering = new Set<string>();
  private readonly legacyContinuationColumn: boolean;
  private readonly probeEpoch = crypto.randomUUID();
  constructor(private readonly storage: DurableObjectStorage,
    private readonly registeredTools: Record<string, NamedTool>,
    private readonly externalTurn: (context: ToolContext) => string | undefined,
    private readonly deliverFinal: DeliverFinalToolResult,
    private readonly waitUntil: (work: Promise<unknown>) => void,
    private readonly replaySafeTools: ReadonlySet<string> = new Set(),
    private readonly wakeGeneration: number = WAKE_GENERATION,
    private readonly activeOutputStatus?: OutputStatus,
    private readonly idleOutputStatus?: OutputStatus,
    private readonly deliverIdleBatch?: DeliverFinalToolResults,
    private readonly maxDatabaseBytes = MAX_ASYNC_DATABASE_BYTES) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS async_jobs (
      id TEXT PRIMARY KEY, invocation TEXT NOT NULL UNIQUE, original_turn TEXT NOT NULL,
      execution_turn TEXT, call_id TEXT, tool TEXT NOT NULL, args TEXT NOT NULL,
      state TEXT NOT NULL, result TEXT, terminal_state TEXT, attempts INTEGER NOT NULL DEFAULT 0,
      started_at INTEGER, created_at INTEGER NOT NULL, delivered_at INTEGER, continuation_started INTEGER, wake_generation INTEGER NOT NULL DEFAULT -1, lease_id TEXT, context_json TEXT, replay_safe INTEGER NOT NULL DEFAULT 0,
      integration_probe_epoch TEXT
    )`);
    // Pilot rows used synthetic user turns, not typed tool results. Preserve
    // their status but never replay them into the new same-call-ID path.
    const columns = new Set(storage.sql.exec<{ name: string }>("PRAGMA table_info(async_jobs)")
      .toArray().map(column => column.name));
    this.legacyContinuationColumn = columns.has("continuation_turn");
    for (const [name, kind] of [["execution_turn", "TEXT"], ["call_id", "TEXT"], ["delivered_at", "INTEGER"], ["continuation_started", "INTEGER"], ["wake_generation", "INTEGER NOT NULL DEFAULT -1"], ["lease_id", "TEXT"], ["context_json", "TEXT"], ["replay_safe", "INTEGER NOT NULL DEFAULT 0"],
      ["integration_probe_epoch", "TEXT"]] as const) {
      if (!columns.has(name)) storage.sql.exec(`ALTER TABLE async_jobs ADD COLUMN ${name} ${kind}`);
    }
    storage.sql.exec("UPDATE async_jobs SET state = 'legacy_uninjectable' WHERE execution_turn IS NULL");
    storage.sql.exec("CREATE INDEX IF NOT EXISTS async_jobs_state ON async_jobs(state)");
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS async_jobs_reconcile_cursor (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1), created_at INTEGER NOT NULL, id TEXT NOT NULL
    )`);
    storage.sql.exec("INSERT OR IGNORE INTO async_jobs_reconcile_cursor VALUES (1, -1, '')");
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS async_jobs_storage_budget (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1), peak_bytes INTEGER NOT NULL
    )`);
    storage.sql.exec("INSERT OR IGNORE INTO async_jobs_storage_budget VALUES (1, 0)");
    // Keep an immutable, compact replay fence after delivered payloads expire.
    // An old tool invocation must never become a new mutable side effect.
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS async_job_tombstones (
      id TEXT PRIMARY KEY, invocation TEXT NOT NULL UNIQUE, tool TEXT NOT NULL,
      archived_at INTEGER NOT NULL
    )`);
    // Deployment alone does not wake an idle DO. On its next construction,
    // recheck one old checkpoint or an output parked against an older kernel.
    // If the capability is still absent, park again without a polling alarm.
    if (storage.sql.exec<{ n: number }>(
      `SELECT COUNT(*) AS n FROM async_jobs WHERE
        state IN ('queued', 'running', 'completed', 'failed', 'uncertain', 'cancelled')
        OR (state = 'awaiting_integration' AND (integration_probe_epoch IS NULL OR integration_probe_epoch != ?))
        OR (state = 'checkpointed' AND wake_generation < ? AND ${terminalOrigin})`,
      this.probeEpoch, this.wakeGeneration).toArray()[0]!.n > 0) {
      this.waitUntil((async () => {
        const nextAt = Date.now() + 1_000;
        const alarm = await storage.getAlarm();
        if (alarm === null || alarm > nextAt) await storage.setAlarm(nextAt);
      })());
    }
  }

  private recordHighWater(): number {
    const bytes = this.storage.sql.databaseSize;
    if (!Number.isSafeInteger(bytes) || bytes < 0) throw new Error("async storage size unavailable");
    this.storage.sql.exec("UPDATE async_jobs_storage_budget SET peak_bytes = ? WHERE singleton = 1 AND peak_bytes < ?", bytes, bytes);
    return this.storage.sql.exec<{ peak_bytes: number }>(
      "SELECT peak_bytes FROM async_jobs_storage_budget WHERE singleton = 1").toArray()[0]!.peak_bytes;
  }

  private archiveDelivered(): void {
    // Capture growth from native journals and finished payloads *before*
    // archival releases their pages; a cold reconcile must not forget the
    // high-water even when its live DB size subsequently shrinks.
    this.recordHighWater();
    const cutoff = Date.now() - RETENTION_MS;
    this.storage.transactionSync(() => {
      this.storage.sql.exec(`INSERT INTO async_job_tombstones (id, invocation, tool, archived_at)
        SELECT id, invocation, tool, ? FROM async_jobs
        WHERE state = 'delivered' AND delivered_at IS NOT NULL AND delivered_at < ?`, Date.now(), cutoff);
      this.storage.sql.exec("DELETE FROM async_jobs WHERE state = 'delivered' AND delivered_at IS NOT NULL AND delivered_at < ?", cutoff);
    });
  }

  private get(id: string): Job | undefined {
    return this.storage.sql.exec<Job>("SELECT * FROM async_jobs WHERE id = ?", id).toArray()[0];
  }
  public status(id: string): { job_id: string; state: string; tool: string; result?: string; continuation_started?: boolean } | undefined {
    const job = this.get(id);
    if (!job) {
      const archived = this.storage.sql.exec<{ tool: string }>(
        "SELECT tool FROM async_job_tombstones WHERE id = ?", id).toArray()[0];
      return archived ? { job_id: id, state: "archived", tool: archived.tool,
        continuation_started: true } : undefined;
    }
    return { job_id: job.id, state: job.state, tool: job.tool,
      ...(job.result === null ? {} : { result: bound(job.result) }),
      ...(job.continuation_started === null ? {} : { continuation_started: job.continuation_started === 1 }) };
  }
  public list(): ReturnType<AsyncJobs["status"]>[] {
    return this.storage.sql.exec<{ id: string }>("SELECT id FROM async_jobs ORDER BY created_at DESC LIMIT 50")
      .toArray().map(row => this.status(row.id));
  }
  public tool(tool: NamedTool): NamedTool {
    if (!Object.hasOwn(this.registeredTools, tool.name)) throw new Error("async tool is not registered for this session");
    return { ...tool, handler: (args: unknown, context: ToolContext) => {
      const originalTurn = this.externalTurn(context);
      if (!originalTurn || !context.turnId || !context.callId) throw new Error("async tool call lacks durable turn correlation");
      const input = JSON.stringify(args);
      if (!input || input.length > MAX_INPUT) throw new Error("async tool arguments exceed limit");
      const invocation = `${context.turnId}:${context.callId}`;
      // Only serializable, non-authority-bearing correlation is persisted. The
      // owner-scoped registered handler remains the sole source of permissions.
      const contextJson = JSON.stringify({ parentCallId: context.parentCallId,
        sessionId: context.sessionId, model: context.model, subagent: context.subagent });
      if (contextJson.length > MAX_INPUT) throw new Error("async context exceeds limit");
      const replaySafe = this.replaySafeTools.has(tool.name) ? 1 : 0;
      let job = this.storage.sql.exec<Job>("SELECT * FROM async_jobs WHERE invocation = ?", invocation).toArray()[0];
      if (job && (job.tool !== tool.name || job.args !== input || job.original_turn !== originalTurn
        || job.execution_turn !== context.turnId || job.call_id !== context.callId
        || job.context_json !== contextJson || job.replay_safe !== replaySafe))
        throw new Error("async invocation conflict");
      if (!job) {
        this.archiveDelivered();
        if (this.storage.sql.exec<{ n: number }>(
          "SELECT COUNT(*) AS n FROM async_job_tombstones WHERE invocation = ?", invocation,
        ).toArray()[0]!.n > 0) throw new Error("async invocation archived; unsafe to replay");
        // Check only *new* invocations after existing-ID and tombstone lookup.
        // A sampled high-water survives restarts and later database shrinkage;
        // over-budget sessions continue delivering/recovering older jobs, but
        // never dispatch another mutable side effect to grow the journal.
        const peak = this.recordHighWater();
        if (peak + ASYNC_ADMISSION_HEADROOM_BYTES >= this.maxDatabaseBytes)
          throw new Error("async session storage budget reached; existing results remain recoverable");
        if (this.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM async_jobs")
          .toArray()[0]!.n >= MAX_JOBS || this.storage.sql.exec<{ n: number }>(
          "SELECT COUNT(*) AS n FROM async_jobs WHERE state IN ('queued', 'running')"
        ).toArray()[0]!.n >= MAX_ACTIVE) throw new Error("async job capacity reached");
        const id = crypto.randomUUID();
        // Durable identity precedes read-only work. This is NOT a Rust
        // transcript placeholder; only the trusted adapter may stage it there.
        // Older SQL tables still have a NOT NULL continuation_turn column.
        // Supply an inert value there; never use it as a new user turn ID.
        if (this.legacyContinuationColumn) this.storage.sql.exec(`INSERT INTO async_jobs
          (id, invocation, original_turn, execution_turn, call_id, continuation_turn, tool, args, context_json, replay_safe, state, created_at)
          VALUES (?, ?, ?, ?, ?, '', ?, ?, ?, ?, 'queued', ?)`,
        id, invocation, originalTurn, context.turnId, context.callId, tool.name, input, contextJson, replaySafe, Date.now());
        else this.storage.sql.exec(`INSERT INTO async_jobs
          (id, invocation, original_turn, execution_turn, call_id, tool, args, context_json, replay_safe, state, created_at)
          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?)`,
        id, invocation, originalTurn, context.turnId, context.callId, tool.name, input, contextJson, replaySafe, Date.now());
        job = this.get(id)!;
      }
      // Dispatch after the provisional output returns to the model, but do not
      // impose a one-second alarm delay. The durable queued row and alarm fence
      // isolate loss; only explicitly read-only jobs may be retried.
      if (job.state === "queued") this.waitUntil(Promise.resolve().then(() => this.run(job!.id)));
      this.waitUntil(this.storage.setAlarm(Date.now() + 1_000));
      return stageFunctionCallOutput(UNREAL_RUNNING_OUTPUT);
    } };
  }

  public readonly statusTool: NamedTool = {
    name: "async_job_status", description: "Inspect the state and bounded result of a previously issued async tool job by job_id.",
    parameters: { type: "object", properties: { job_id: { type: "string" } }, required: ["job_id"], additionalProperties: false },
    handler: (input: unknown) => {
      const id = (input as { job_id?: unknown } | null)?.job_id;
      return typeof id === "string" && /^[0-9a-f-]{36}$/.test(id)
        ? this.status(id) ?? { error: "not_found" } : { error: "invalid_job_id" };
    },
  };

  /** Cancellation is a durable fence, not a claim that an in-flight side
   * effect was rolled back. A late handler result cannot overwrite it. */
  public async cancel(id: string): Promise<ReturnType<AsyncJobs["status"]>> {
    const job = this.get(id);
    if (!job) return undefined;
    if (job.state === "queued") this.storage.sql.exec(`UPDATE async_jobs SET state = 'cancelled',
      terminal_state = 'cancelled', result = 'Job cancelled before dispatch' WHERE id = ? AND state = 'queued'`, id);
    else if (job.state === "running") this.storage.sql.exec(`UPDATE async_jobs SET state = 'uncertain',
      terminal_state = 'uncertain', result = 'Cancellation requested after dispatch; side effect may have occurred'
      WHERE id = ? AND state = 'running'`, id);
    await this.storage.setAlarm(Date.now() + 1_000);
    return this.status(id);
  }

  private async run(id: string): Promise<void> {
    const job = this.get(id);
    const expired = job?.state === "running" && job.started_at !== null
      && job.started_at + LEASE_MS < Date.now();
    // A handler in this same DO may hang beyond its durable lease. Fence its
    // mutable outcome, or let a read-only attempt supersede that lease. The
    // old handler's conditional write cannot overwrite the winning result.
    if (this.active.has(id) && !expired) return;
    if (!job || (job.state !== "queued" && !expired)) return;
    // A stale running mutable lease could already have executed. Even an
    // apparently idempotent shell command is never replayed without proof.
    if (job.state === "running" && !job.replay_safe) {
      this.storage.sql.exec(`UPDATE async_jobs SET state = 'uncertain', terminal_state = 'uncertain', result = ?
        WHERE id = ? AND state = 'running' AND lease_id = ?`,
        "Execution outcome unknown after worker interruption; do not retry the side effect", id, job.lease_id);
      await this.storage.setAlarm(Date.now() + 1_000);
      return;
    }
    if (job.attempts >= MAX_ATTEMPTS) {
      this.storage.sql.exec(`UPDATE async_jobs SET state = 'failed', terminal_state = 'failed', result = ?
        WHERE id = ? AND (state = 'queued' OR (state = 'running' AND started_at + ? < ?))`,
        "Replay-safe job retry limit reached", id, LEASE_MS, Date.now());
      await this.storage.setAlarm(Date.now() + 1_000);
      return;
    }
    const attempt = job.attempts + 1;
    const leaseId = crypto.randomUUID();
    this.storage.sql.exec(`UPDATE async_jobs SET state = 'running', attempts = ?, started_at = ?, lease_id = ?
      WHERE id = ? AND attempts = ? AND (state = 'queued' OR (state = 'running' AND started_at + ? < ?))`,
      attempt, Date.now(), leaseId, id, job.attempts, LEASE_MS, Date.now());
    if (this.get(id)?.lease_id !== leaseId) return;
    this.active.set(id, leaseId);
    try {
      const tool = this.registeredTools[job.tool];
      if (!tool) throw new Error("registered tool no longer available");
      const correlation = JSON.parse(job.context_json ?? "{}");
      const context: ToolContext = { callId: job.call_id!, parentCallId: correlation.parentCallId ?? "",
        sessionId: correlation.sessionId ?? "async-job", turnId: job.execution_turn!,
        model: correlation.model ?? "async-job", ...(correlation.subagent ? { subagent: correlation.subagent } : {}),
        signal: new AbortController().signal };
      const output = JSON.stringify(await tool.handler(JSON.parse(job.args), context)) ?? "null";
      if (output.length > MAX_DURABLE_RESULT) throw new Error("tool output exceeds durable limit");
      this.storage.sql.exec("UPDATE async_jobs SET state = 'completed', terminal_state = 'completed', result = ? WHERE id = ? AND state = 'running' AND attempts = ? AND lease_id = ?",
        output, id, attempt, leaseId);
    } catch {
      // A mutable tool may have committed before throwing or losing its reply.
      // Preserve uncertainty rather than presenting failure as rollback.
      const terminal = job.replay_safe ? "failed" : "uncertain";
      this.storage.sql.exec(`UPDATE async_jobs SET state = ?, terminal_state = ?, result = ?
        WHERE id = ? AND state = 'running' AND attempts = ? AND lease_id = ?`,
        terminal, terminal, job.replay_safe ? "Replay-safe job failed (details withheld)"
          : "Execution outcome unknown; side effect may have occurred", id, attempt, leaseId);
    } finally {
      if (this.active.get(id) === leaseId) this.active.delete(id);
      await this.storage.setAlarm(Date.now() + 1_000);
    }
  }

  public async reconcile(): Promise<void> {
    this.archiveDelivered();
    // All running work is examined on every tick. Terminal records rotate
    // through a durable keyset, so neither a page of unconfirmed checkpoints
    // nor a page of ready results can starve the other after DO eviction.
    const activeRows = this.storage.sql.exec<Job>(
      "SELECT * FROM async_jobs WHERE state IN ('queued', 'running') ORDER BY created_at, id LIMIT ?",
      MAX_ACTIVE).toArray();
    const cursor = this.storage.sql.exec<{ created_at: number; id: string }>(
      "SELECT created_at, id FROM async_jobs_reconcile_cursor WHERE singleton = 1",
    ).toArray()[0]!;
    const eligible = `state NOT IN ('queued', 'running', 'delivered', 'legacy_uninjectable')
      AND (state != 'checkpointed' OR (wake_generation < ? AND ${terminalOrigin}))
      AND (state != 'awaiting_integration' OR integration_probe_epoch IS NULL OR integration_probe_epoch != ?)`;
    const capacity = 25 - activeRows.length;
    const terminalRows = this.storage.sql.exec<Job>(
      `SELECT * FROM async_jobs WHERE ${eligible}
        AND (created_at > ? OR (created_at = ? AND id > ?))
        ORDER BY created_at, id LIMIT ?`,
      this.wakeGeneration, this.probeEpoch, cursor.created_at, cursor.created_at, cursor.id, capacity,
    ).toArray();
    if (terminalRows.length < capacity) {
      terminalRows.push(...this.storage.sql.exec<Job>(
        `SELECT * FROM async_jobs WHERE ${eligible}
          AND (created_at < ? OR (created_at = ? AND id <= ?))
          ORDER BY created_at, id LIMIT ?`,
        this.wakeGeneration, this.probeEpoch, cursor.created_at, cursor.created_at, cursor.id,
        capacity - terminalRows.length,
      ).toArray());
    }
    if (terminalRows.length > 0) {
      const last = terminalRows.at(-1)!;
      // Commit the cursor before any adapter await; a crash can delay a job
      // one finite rotation, but cannot pin the scan to a hot prefix.
      this.storage.sql.exec("UPDATE async_jobs_reconcile_cursor SET created_at = ?, id = ? WHERE singleton = 1",
        last.created_at, last.id);
    }
    const rows = [...activeRows, ...terminalRows];
    let retry = false;
    const idleCohort: { job: Job; intent: FinalToolResultIntent }[] = [];
    const wakingSources = new Set<string>();
    for (const job of rows) {
      if (job.state === "queued" || job.state === "running") {
        this.waitUntil(this.run(job.id));
        retry = true;
        continue;
      }
      if (!["completed", "failed", "uncertain", "cancelled", "awaiting_integration", "checkpointed"].includes(job.state)) continue;
      const terminalState = job.terminal_state;
      if (!job.execution_turn || !job.call_id || job.result === null
        || (terminalState !== "completed" && terminalState !== "failed"
          && terminalState !== "uncertain" && terminalState !== "cancelled")) continue;
      if (this.delivering.has(job.id)) continue;
      this.delivering.add(job.id);
      let heldForBatch = false;
      try {
        const intent: FinalToolResultIntent = { originalTurn: job.original_turn, executionTurn: job.execution_turn,
          callId: job.call_id, tool: job.tool, jobId: job.id, terminalState, output: job.result };
        if (job.state === "checkpointed" && this.activeOutputStatus) {
          const active = await this.activeOutputStatus(intent) as {
            state?: unknown; model_call_index?: unknown; response_id?: unknown
          } | null;
          const activeConfirmed = active?.state === "confirmed"
            && Number.isSafeInteger(active.model_call_index) && (active.model_call_index as number) > 0
            && typeof active.response_id === "string" && active.response_id.length > 0;
          const idle = !activeConfirmed && this.idleOutputStatus ? await this.idleOutputStatus(intent) as {
            state?: unknown; model_call_index?: unknown; response_id?: unknown
          } | null : null;
          const confirmed = [active, idle].some(status => status?.state === "confirmed"
            && Number.isSafeInteger(status.model_call_index) && (status.model_call_index as number) > 0
            && typeof status.response_id === "string" && status.response_id.length > 0);
          if (confirmed) {
            // Exactly this job/call was consumed by an authoritative completed
            // active or idle model step, never merely submitted to the actor.
            this.storage.sql.exec(`UPDATE async_jobs SET state = 'delivered', continuation_started = 1,
              delivered_at = ?, wake_generation = ? WHERE id = ? AND state = 'checkpointed'`,
              Date.now(), this.wakeGeneration, job.id);
            continue;
          }
          if ([active, idle].some(status => status?.state === "accepted_unbound"
            || status?.state === "bound_unconfirmed")) {
            // The provider is still working. Do not re-submit while either
            // durable owner retains an unconfirmed exact receipt. A later
            // same-source cohort must not race that in-flight native wake.
            wakingSources.add(job.original_turn);
            retry = true;
            continue;
          }
          if (idle?.state === "discarded") {
            // An old completed late-output journal may make a retry return a
            // stale checkpoint. Keep the explicit failure of uptake visible.
            this.storage.sql.exec("UPDATE async_jobs SET wake_generation = ? WHERE id = ? AND state = 'checkpointed'",
              this.wakeGeneration, job.id);
            continue;
          }
          if (active?.state !== "discarded" || (this.idleOutputStatus && idle?.state !== "pruned_or_unknown")) {
            // Pruned/unknown is not safe to replay. Never infer consumption.
            this.storage.sql.exec("UPDATE async_jobs SET wake_generation = ? WHERE id = ? AND state = 'checkpointed'",
              this.wakeGeneration, job.id);
            continue;
          }
          // The active owner explicitly discarded the output, with no idle
          // receipt. Re-submit the identical operation at an idle boundary.
        }
        // Capture settlement before awaiting the Rust adapter. A cancellation
        // can race a slow receipt; observing only afterwards would suppress
        // the necessary idle reconciliation of that active acceptance.
        const settledBefore = this.storage.sql.exec<{ n: number }>(
          `SELECT COUNT(*) AS n FROM async_jobs WHERE id = ? AND ${terminalOrigin}`, job.id,
        ).toArray()[0]!.n > 0;
        if (this.deliverIdleBatch && settledBefore) {
          // A SQL terminal source alone is insufficient if another user turn
          // is pending/accepted. Native admission proves actual actor idleness
          // again, closing the race with turn admission across this await.
          const activeTurns = this.storage.sql.exec<{ n: number }>(
            "SELECT COUNT(*) AS n FROM turns WHERE state IN ('pending', 'accepted')",
          ).toArray()[0]!.n;
          if (activeTurns === 0) {
            idleCohort.push({ job, intent });
            heldForBatch = true;
            continue;
          }
          retry = true;
          continue;
        }
        // Stable intent across ambiguous failures; only the Rust adapter can
        // decide whether the pending output was sent and dedupe terminal output.
        const rawReceipt = await this.deliverFinal(intent);
        // A false receipt is acceptance, never proof of provider uptake. If
        // accepted during the original active turn, leave it eligible for one
        // identical-operation reconciliation after that turn settles. The
        // durable Rust journal deduplicates an already delivered terminal.
        // Neither outcome by itself upgrades the status to delivered.
        const receipt = rawReceipt as Partial<FinalToolResultReceipt> | null | undefined;
        if (receipt?.operation_id !== job.id || receipt.call_id !== job.call_id
          || typeof receipt.replayed !== "boolean" || typeof receipt.continuation_started !== "boolean") {
          throw new Error("invalid terminal output checkpoint receipt");
        }
        this.storage.sql.exec(`UPDATE async_jobs SET state = ?, delivered_at = ?, continuation_started = ?, wake_generation = ?
          WHERE id = ? AND state IN ('completed', 'failed', 'uncertain', 'cancelled', 'awaiting_integration', 'checkpointed')`,
          "checkpointed", Date.now(), 0,
          // An idle status check is still needed after a cold DO restart if
          // this invocation dies between the SQL commit and alarm rearm.
          this.idleOutputStatus ? 0 : job.state === "checkpointed" && settledBefore
            ? this.wakeGeneration : 0, job.id);
        // A continuation-start hint is not an uptake receipt. Reconcile the
        // exact durable model step on the next alarm, including after cold DO
        // restart; don't claim delivery on a successful submission alone.
        if (this.idleOutputStatus && settledBefore) retry = true;
      } catch (error) {
        if (error instanceof TypedIngestionUnavailable) {
          this.storage.sql.exec("UPDATE async_jobs SET state = 'awaiting_integration', integration_probe_epoch = ? WHERE id = ?",
            this.probeEpoch, job.id);
        } else retry = true;
      } finally {
        if (!heldForBatch) this.delivering.delete(job.id);
      }
    }
    // A completed row may be in this bounded page while an older unconfirmed
    // same-source checkpoint lies beyond it. The SQL fence prevents chunk 2
    // from racing a prompt-less wake even across an eviction or full page.
    const eligibleIds = new Set(idleCohort.map(({ job }) => job.id));
    for (const row of this.storage.sql.exec<{ id: string; original_turn: string }>(
      `SELECT id, original_turn FROM async_jobs WHERE state = 'checkpointed'
        AND wake_generation < ? AND ${terminalOrigin}`,
      this.wakeGeneration).toArray()) {
      if (!eligibleIds.has(row.id)) wakingSources.add(row.original_turn);
    }
    // Cohorts are source-turn-local and ordered by creation time and job ID;
    // each native call is bounded to eight outputs and has only original IDs.
    // The native batch validates/stages all members before one idle wake. No
    // individual fallback is allowed for an idle candidate on an old kernel.
    const cohorts = new Map<string, typeof idleCohort>();
    for (const entry of idleCohort) {
      const cohort = cohorts.get(entry.job.original_turn) ?? [];
      cohort.push(entry);
      cohorts.set(entry.job.original_turn, cohort);
    }
    for (const cohort of cohorts.values()) {
      cohort.sort((a, b) => a.job.created_at - b.job.created_at || a.job.id.localeCompare(b.job.id));
      // The native driver can accept only one bounded idle batch per wake.
      // Never send chunk 2 while chunk 1's model call is still in flight.
      if (wakingSources.has(cohort[0]!.job.original_turn)) {
        retry = true;
        for (const { job } of cohort) this.delivering.delete(job.id);
        continue;
      }
      const batch = cohort.slice(0, 8);
      if (cohort.length > batch.length) {
        retry = true;
        for (const { job } of cohort.slice(batch.length)) this.delivering.delete(job.id);
      }
      try {
        const result = await this.deliverIdleBatch!(batch.map(row => row.intent));
        const receipts = result as Partial<FinalToolResultReceipt>[] | null;
        if (!Array.isArray(receipts) || receipts.length !== batch.length
          || receipts.some((receipt, index) => receipt?.operation_id !== batch[index]!.job.id
            || receipt.call_id !== batch[index]!.job.call_id
            || typeof receipt.replayed !== "boolean"
            || typeof receipt.continuation_started !== "boolean")) {
          throw new Error("invalid terminal output batch checkpoint receipts");
        }
        // Keep the local cohort transition all-or-nothing across a DO crash;
        // only exact durable uptake status may later mark each job delivered.
        this.storage.transactionSync(() => {
          for (const { job } of batch) this.storage.sql.exec(`UPDATE async_jobs SET
            state = 'checkpointed', delivered_at = ?, continuation_started = 0,
            wake_generation = 0 WHERE id = ?
            AND state IN ('completed', 'failed', 'uncertain', 'cancelled', 'awaiting_integration', 'checkpointed')`,
            Date.now(), job.id);
        });
        if (this.idleOutputStatus) retry = true;
      } catch (error) {
        if (error instanceof TypedIngestionUnavailable) {
          for (const { job } of batch) this.storage.sql.exec(
            "UPDATE async_jobs SET state = 'awaiting_integration', integration_probe_epoch = ? WHERE id = ?",
            this.probeEpoch, job.id);
        } else retry = true;
      } finally {
        for (const { job } of batch) this.delivering.delete(job.id);
      }
    }
    // Capture the Rust journal's growth after output admission/model wake,
    // not only the host job payload before archival. This high-water remains
    // even if a later native compaction reduces the live database size.
    this.recordHighWater();
    // A checkpointed active acceptance is retried only after its source turn
    // settles. One post-settlement attempt is parked without a poll loop;
    // its false receipt still cannot prove model uptake.
    if (retry || this.storage.sql.exec<{ n: number }>(
      `SELECT COUNT(*) AS n FROM async_jobs WHERE state IN
        ('queued', 'running', 'completed', 'failed', 'uncertain', 'cancelled')
        OR (state = 'checkpointed' AND wake_generation < ? AND ${terminalOrigin})
        OR (state = 'awaiting_integration' AND (integration_probe_epoch IS NULL OR integration_probe_epoch != ?))`,
      this.wakeGeneration, this.probeEpoch,
    ).toArray()[0]!.n > 0) await this.storage.setAlarm(Date.now() + 1_000);
  }
}
