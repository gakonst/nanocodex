import type { NamedTool, ToolContext } from "nanocodex";
import { stageFunctionCallOutput } from "../../nanocodex/host/internal-Agent.mjs";

/** Only explicitly registered read-only tools may outlive a model call. */
// Unreal Agent's MIT-licensed context-builder pending output; source:
// https://github.com/unreallabsai/unreal-agent/blob/1b9f778453f411c029b39b85102aaefb95e7e48d/harness/contextbuilder/builder.go
export const UNREAL_RUNNING_OUTPUT = "Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it.";
const MAX_INPUT = 64_000;
const MAX_RESULT = 8_192;
const LEASE_MS = 30_000;
const MAX_ATTEMPTS = 3;
const MAX_JOBS = 100;
const MAX_ACTIVE = 8;
const RETENTION_MS = 7 * 24 * 60 * 60 * 1000;
const bound = (value: string) => value.length > MAX_RESULT ? `${value.slice(0, MAX_RESULT)}\n[truncated]` : value;

type Job = { id: string; invocation: string; original_turn: string; execution_turn: string | null; call_id: string | null;
  tool: string; args: string; state: string; result: string | null; attempts: number; started_at: number | null;
  terminal_state: string | null; delivered_at: number | null; lease_id: string | null };
/** Durable intent, NOT a provider output. ToolContext.turnId identifies a JS
 * execution; it must not be assumed to identify a Rust Agent turn. */
export type FinalToolResultIntent = Readonly<{ originalTurn: string; executionTurn: string; callId: string;
  tool: string; jobId: string; terminalState: "completed" | "failed"; output: string }>;
/** A trusted adapter must resolve the original Agent tool call, atomically
 * replace its unsent pending output OR append the terminal output under the
 * same call ID if pending was already sent, and durably dedupe jobId.
 * Never implement this with turn.prompt(). */
export type DeliverFinalToolResult = (intent: FinalToolResultIntent) => Promise<void>;
export class TypedIngestionUnavailable extends Error {
  constructor() { super("typed same-call-ID result ingestion is not available"); }
}

export class AsyncJobs {
  private readonly active = new Set<string>();
  private readonly legacyContinuationColumn: boolean;
  constructor(private readonly storage: DurableObjectStorage,
    private readonly readonlyTools: Record<string, NamedTool>,
    private readonly externalTurn: (context: ToolContext) => string | undefined,
    private readonly deliverFinal: DeliverFinalToolResult,
    private readonly waitUntil: (work: Promise<unknown>) => void) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS async_jobs (
      id TEXT PRIMARY KEY, invocation TEXT NOT NULL UNIQUE, original_turn TEXT NOT NULL,
      execution_turn TEXT, call_id TEXT, tool TEXT NOT NULL, args TEXT NOT NULL,
      state TEXT NOT NULL, result TEXT, terminal_state TEXT, attempts INTEGER NOT NULL DEFAULT 0,
      started_at INTEGER, created_at INTEGER NOT NULL, delivered_at INTEGER, lease_id TEXT
    )`);
    // Pilot rows used synthetic user turns, not typed tool results. Preserve
    // their status but never replay them into the new same-call-ID path.
    const columns = new Set(storage.sql.exec<{ name: string }>("PRAGMA table_info(async_jobs)")
      .toArray().map(column => column.name));
    this.legacyContinuationColumn = columns.has("continuation_turn");
    for (const [name, kind] of [["execution_turn", "TEXT"], ["call_id", "TEXT"], ["delivered_at", "INTEGER"], ["lease_id", "TEXT"]] as const) {
      if (!columns.has(name)) storage.sql.exec(`ALTER TABLE async_jobs ADD COLUMN ${name} ${kind}`);
    }
    storage.sql.exec("UPDATE async_jobs SET state = 'legacy_uninjectable' WHERE execution_turn IS NULL");
    storage.sql.exec("CREATE INDEX IF NOT EXISTS async_jobs_state ON async_jobs(state)");
  }

  private get(id: string): Job | undefined {
    return this.storage.sql.exec<Job>("SELECT * FROM async_jobs WHERE id = ?", id).toArray()[0];
  }
  public status(id: string): { job_id: string; state: string; tool: string; result?: string } | undefined {
    const job = this.get(id);
    if (!job) return undefined;
    return { job_id: job.id, state: job.state, tool: job.tool,
      ...(job.result === null ? {} : { result: job.result }) };
  }
  public list(): ReturnType<AsyncJobs["status"]>[] {
    return this.storage.sql.exec<{ id: string }>("SELECT id FROM async_jobs ORDER BY created_at DESC LIMIT 50")
      .toArray().map(row => this.status(row.id));
  }
  public tool(tool: NamedTool): NamedTool {
    if (!Object.hasOwn(this.readonlyTools, tool.name)) throw new Error("async tool is not allowlisted");
    return { ...tool, handler: (args: unknown, context: ToolContext) => {
      const originalTurn = this.externalTurn(context);
      if (!originalTurn || !context.turnId || !context.callId) throw new Error("async tool call lacks durable turn correlation");
      const input = JSON.stringify(args);
      if (!input || input.length > MAX_INPUT) throw new Error("async tool arguments exceed limit");
      const invocation = `${context.turnId}:${context.callId}`;
      let job = this.storage.sql.exec<Job>("SELECT * FROM async_jobs WHERE invocation = ?", invocation).toArray()[0];
      if (job && (job.tool !== tool.name || job.args !== input || job.original_turn !== originalTurn
        || job.execution_turn !== context.turnId || job.call_id !== context.callId))
        throw new Error("async invocation conflict");
      if (!job) {
        this.storage.sql.exec("DELETE FROM async_jobs WHERE state = 'delivered' AND created_at < ?",
          Date.now() - RETENTION_MS);
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
          (id, invocation, original_turn, execution_turn, call_id, continuation_turn, tool, args, state, created_at)
          VALUES (?, ?, ?, ?, ?, '', ?, ?, 'queued', ?)`,
        id, invocation, originalTurn, context.turnId, context.callId, tool.name, input, Date.now());
        else this.storage.sql.exec(`INSERT INTO async_jobs
          (id, invocation, original_turn, execution_turn, call_id, tool, args, state, created_at)
          VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', ?)`,
        id, invocation, originalTurn, context.turnId, context.callId, tool.name, input, Date.now());
        job = this.get(id)!;
      }
      // Dispatch after the provisional output returns to the model, but do not
      // impose a one-second alarm delay. The durable queued row and alarm fence
      // isolate loss; only explicitly read-only jobs may be retried.
      this.waitUntil(Promise.resolve().then(() => this.run(job!.id)));
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

  private async run(id: string): Promise<void> {
    if (this.active.has(id)) return;
    const job = this.get(id);
    if (!job || (job.state !== "queued" && !(job.state === "running" &&
      job.started_at !== null && job.started_at + LEASE_MS < Date.now()))) return;
    if (job.attempts >= MAX_ATTEMPTS) {
      this.storage.sql.exec(`UPDATE async_jobs SET state = 'failed', terminal_state = 'failed', result = ?
        WHERE id = ? AND (state = 'queued' OR (state = 'running' AND started_at + ? < ?))`,
        "Read-only job retry limit reached", id, LEASE_MS, Date.now());
      await this.storage.setAlarm(Date.now() + 1_000);
      return;
    }
    this.active.add(id);
    const attempt = job.attempts + 1;
    const leaseId = crypto.randomUUID();
    this.storage.sql.exec(`UPDATE async_jobs SET state = 'running', attempts = ?, started_at = ?, lease_id = ?
      WHERE id = ? AND attempts = ? AND (state = 'queued' OR (state = 'running' AND started_at + ? < ?))`,
      attempt, Date.now(), leaseId, id, job.attempts, LEASE_MS, Date.now());
    if (this.get(id)?.lease_id !== leaseId) { this.active.delete(id); return; }
    try {
      const tool = this.readonlyTools[job.tool];
      if (!tool) throw new Error("read-only tool no longer allowlisted");
      // Never persist/replay an execution-capable tool. No provider or parent
      // call context is propagated into owner-scoped read-only bindings.
      const context = { callId: job.call_id!, parentCallId: "", sessionId: "async-job",
        turnId: job.execution_turn!, model: "async-job", signal: new AbortController().signal };
      const output = await tool.handler(JSON.parse(job.args), context);
      this.storage.sql.exec("UPDATE async_jobs SET state = 'completed', terminal_state = 'completed', result = ? WHERE id = ? AND state = 'running' AND attempts = ? AND lease_id = ?",
        bound(JSON.stringify(output) ?? "null"), id, attempt, leaseId);
    } catch {
      this.storage.sql.exec("UPDATE async_jobs SET state = 'failed', terminal_state = 'failed', result = ? WHERE id = ? AND state = 'running' AND attempts = ? AND lease_id = ?",
        "Read-only job failed (details withheld)", id, attempt, leaseId);
    } finally {
      this.active.delete(id);
      await this.storage.setAlarm(Date.now() + 1_000);
    }
  }

  public async reconcile(): Promise<void> {
    this.storage.sql.exec("DELETE FROM async_jobs WHERE state = 'delivered' AND created_at < ?",
      Date.now() - RETENTION_MS);
    const rows = this.storage.sql.exec<Job>(
      "SELECT * FROM async_jobs WHERE state NOT IN ('delivered', 'legacy_uninjectable') ORDER BY (state = 'awaiting_integration'), created_at LIMIT 25",
    ).toArray();
    let retry = false;
    for (const job of rows) {
      if (job.state === "queued" || job.state === "running") {
        this.waitUntil(this.run(job.id));
        retry = true;
        continue;
      }
      if (!["completed", "failed", "awaiting_integration"].includes(job.state)) continue;
      if (!job.execution_turn || !job.call_id || job.result === null
        || (job.terminal_state !== "completed" && job.terminal_state !== "failed")) continue;
      try {
        // Stable intent across ambiguous failures; only the Rust adapter can
        // decide whether the pending output was sent and dedupe terminal output.
        await this.deliverFinal({ originalTurn: job.original_turn, executionTurn: job.execution_turn,
          callId: job.call_id, tool: job.tool, jobId: job.id,
          terminalState: job.terminal_state, output: job.result });
        this.storage.sql.exec("UPDATE async_jobs SET state = 'delivered', delivered_at = ? WHERE id = ? AND state IN ('completed', 'failed', 'awaiting_integration')",
          Date.now(), job.id);
      } catch (error) {
        if (error instanceof TypedIngestionUnavailable) {
          this.storage.sql.exec("UPDATE async_jobs SET state = 'awaiting_integration' WHERE id = ?", job.id);
        } else retry = true;
      }
    }
    // Do not spin an alarm forever when typed ingestion has not been shipped.
    if (retry || this.storage.sql.exec<{ n: number }>(
      "SELECT COUNT(*) AS n FROM async_jobs WHERE state IN ('queued', 'running', 'completed', 'failed')"
    ).toArray()[0]!.n > 0) await this.storage.setAlarm(Date.now() + 1_000);
  }
}
