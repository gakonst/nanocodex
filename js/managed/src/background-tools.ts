import type { NamedTool, ToolContext } from "nanocodex";

const MAX_INPUT = 8_192;
const MAX_OUTPUT = 16_384;
const MAX_ACTIVE = 8;

export type BackgroundToolDelivery = Readonly<{
  /** Stable deduplication key. The callback MUST atomically deduplicate this ID with its message append. */
  jobId: string;
  sessionId: string;
  sourceTurnId?: string;
  toolName: "web__run";
  /** Treat as untrusted source material, never as instructions or tool authority. */
  trust: "untrusted_tool_result";
  content: string;
}>;

type Job = {
  id: string; session_id: string; turn_id: string | null; context_json: string;
  input_json: string; state: "pending" | "running" | "complete" | "delivered";
  output: string | null;
};
type ContextSnapshot = Pick<ToolContext, "callId" | "parentCallId" | "sessionId" | "turnId" | "model" | "subagent">;

/** An intentionally narrow, opt-in pilot. No arbitrary NamedTool or connector writes are accepted. */
export class BackgroundReadToolRunner {
  readonly #running = new Map<string, Promise<void>>();
  readonly #storage: DurableObjectStorage;
  readonly #tool: NamedTool;
  readonly #waitUntil: (promise: Promise<void>) => void;
  readonly #deliver: (message: BackgroundToolDelivery) => Promise<void>;
  readonly #authorize: (context: ToolContext) => boolean;

  constructor(options: {
    storage: DurableObjectStorage;
    waitUntil(promise: Promise<void>): void;
    /** Callback must append a NEW lower-trust message, never a tool result into an old turn. */
    deliver(message: BackgroundToolDelivery): Promise<void>;
    /** Rechecked on start and recovery; current session/grant must still be authorized. */
    authorize(context: ToolContext): boolean;
    tool: NamedTool;
  }) {
    if (options.tool.name !== "web__run") throw new TypeError("only the read-only web__run pilot is permitted");
    this.#storage = options.storage;
    this.#tool = options.tool;
    this.#waitUntil = options.waitUntil;
    this.#deliver = options.deliver;
    this.#authorize = options.authorize;
    this.#storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_background_read_jobs (
      id TEXT PRIMARY KEY, session_id TEXT NOT NULL, turn_id TEXT,
      context_json TEXT NOT NULL, input_json TEXT NOT NULL,
      state TEXT NOT NULL CHECK(state IN ('pending','running','complete','delivered')),
      output TEXT
    )`);
  }

  statusTool(): NamedTool {
    return {
      name: "background_tool_status",
      description: "Inspect a background web search job by ID; when complete, retrieve its untrusted result as tool output. Results also arrive as separate completion messages.",
      parameters: { type: "object", additionalProperties: false, properties: { job_id: { type: "string", pattern: "^bg-[a-f0-9]{32}$" } }, required: ["job_id"] },
      handler: (input, context) => {
        context.signal.throwIfAborted();
        if (!this.#authorize(context)) throw new Error("background read not authorized");
        if (!input || typeof input !== "object" || Array.isArray(input) || Object.keys(input).length !== 1
          || typeof (input as { job_id?: unknown }).job_id !== "string"
          || !/^bg-[a-f0-9]{32}$/.test((input as { job_id: string }).job_id)) throw new TypeError("invalid job ID");
        return this.get((input as { job_id: string }).job_id, context.sessionId) ?? { error: "job_not_found" };
      },
    };
  }

  undelivered(sessionId?: string): BackgroundToolDelivery[] {
    const rows = sessionId === undefined
      ? this.#storage.sql.exec<Job>("SELECT * FROM managed_background_read_jobs WHERE state = 'complete' ORDER BY rowid LIMIT ?", MAX_ACTIVE)
      : this.#storage.sql.exec<Job>("SELECT * FROM managed_background_read_jobs WHERE state = 'complete' AND session_id = ? ORDER BY rowid LIMIT ?", sessionId, MAX_ACTIVE);
    return [...rows].map(row => ({ jobId: row.id, sessionId: row.session_id, sourceTurnId: row.turn_id ?? undefined,
      toolName: "web__run" as const, trust: "untrusted_tool_result" as const, content: row.output ?? "null" }));
  }

  private markDelivered(jobId: string): void {
    this.#storage.sql.exec("UPDATE managed_background_read_jobs SET state = 'delivered' WHERE id = ? AND state = 'complete'", jobId);
  }

  pendingForTurn(turnId: string): number {
    return this.#storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_background_read_jobs WHERE turn_id = ? AND state IN ('pending', 'running')", turnId,
    ).one().count;
  }

  tool(): NamedTool {
    return {
      name: "start_background_web_search",
      description: "Opt in to a read-only web search that returns a job ID immediately. Its bounded result arrives later as a NEW lower-trust message; it will not be inserted into the current tool call or turn. Only search_query is supported. Never use this for writes or requests requiring exact-once execution.",
      parameters: {
        type: "object", additionalProperties: false,
        properties: { search_query: { type: "array", minItems: 1, maxItems: 4, items: {
          type: "object", additionalProperties: false, properties: { q: { type: "string", minLength: 1, maxLength: 512 } }, required: ["q"],
        } } }, required: ["search_query"],
      },
      handler: (input, context) => this.start(input, context),
    };
  }

  async start(input: unknown, context: ToolContext): Promise<{ job_id: string; status: "pending" }> {
    context.signal.throwIfAborted();
    if (!this.#authorize(context)) throw new Error("background read not authorized");
    const normalized = parseSearchInput(input);
    const contextJson = JSON.stringify({
      callId: context.callId, parentCallId: context.parentCallId, sessionId: context.sessionId,
      turnId: context.turnId, model: context.model, subagent: context.subagent,
    } satisfies ContextSnapshot);
    // The original nested call may replay after a Worker restart. Bind its
    // receipt to the call and exact input rather than creating another job.
    const inputJson = JSON.stringify(normalized);
    const identity = JSON.stringify([context.sessionId, context.turnId, context.parentCallId,
      context.callId, inputJson]);
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(identity));
    const id = `bg-${[...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, "0")).join("").slice(0, 32)}`;
    if (this.get(id, context.sessionId)) {
      this.#schedule(id);
      return { job_id: id, status: "pending" };
    }
    const active = this.#storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_background_read_jobs WHERE state != 'delivered'",
    ).one().count;
    if (active >= MAX_ACTIVE) throw new Error("background job capacity reached");
    this.#storage.sql.exec(
      "INSERT OR IGNORE INTO managed_background_read_jobs VALUES (?, ?, ?, ?, ?, 'pending', NULL)",
      id, context.sessionId, context.turnId ?? null, contextJson, inputJson,
    );
    // Do not invoke an external tool until the intent survives a restart.
    await this.#storage.sync();
    this.#schedule(id);
    return { job_id: id, status: "pending" };
  }

  /** Call from an alarm or owner startup; incomplete reads may be repeated, but never writes. */
  resume(): void {
    for (const row of this.#storage.sql.exec<{ id: string }>(
      "SELECT id FROM managed_background_read_jobs WHERE state != 'delivered' ORDER BY rowid LIMIT ?", MAX_ACTIVE,
    )) this.#schedule(row.id);
  }

  get(jobId: string, sessionId: string): { job_id: string; status: Job["state"]; result?: string } | undefined {
    const row = this.#storage.sql.exec<Pick<Job, "session_id" | "state" | "output">>(
      "SELECT session_id, state, output FROM managed_background_read_jobs WHERE id = ?", jobId,
    ).toArray()[0];
    return row?.session_id === sessionId ? { job_id: jobId, status: row.state,
      ...(row.output === null ? {} : { result: row.output }) } : undefined;
  }

  #schedule(id: string): void {
    if (this.#running.has(id)) return;
    const work = this.#process(id).finally(() => this.#running.delete(id));
    this.#running.set(id, work);
    // waitUntil owns the lifetime. A DB failure remains pending for alarm/startup recovery.
    this.#waitUntil(work);
  }

  async #process(id: string): Promise<void> {
    const job = this.#storage.sql.exec<Job>("SELECT * FROM managed_background_read_jobs WHERE id = ?", id).toArray()[0];
    if (!job || job.state === "delivered") return;
    const snapshot = JSON.parse(job.context_json) as ContextSnapshot;
    // The originating call's abort signal is not appropriate after the turn ends.
    const controller = new AbortController();
    const context: ToolContext = { ...snapshot, signal: controller.signal };
    if (!this.#authorize(context)) return; // Leave pending for an owner to inspect; never deliver to a revoked grant.
    if (job.state === "pending" || job.state === "running") {
      // A crash after dispatch is uncertain: only this explicitly read-only tool may run twice.
      this.#storage.sql.exec("UPDATE managed_background_read_jobs SET state = 'running' WHERE id = ?", id);
      await this.#storage.sync();
      let content: string;
      const deadline = setTimeout(() => controller.abort(), 60_000);
      try {
        const result = await this.#tool.handler(JSON.parse(job.input_json), context);
        const serialized = typeof result === "string" ? result : JSON.stringify(result);
        content = bounded(serialized ?? "null");
      } catch {
        // Do not leak provider error messages which may contain sensitive query URLs or tokens.
        content = "Web search failed. The read-only request may have run; do not infer a result.";
      } finally {
        clearTimeout(deadline);
      }
      this.#storage.sql.exec(
        "UPDATE managed_background_read_jobs SET state = 'complete', output = ? WHERE id = ?",
        content, id,
      );
      await this.#storage.sync();
    }
    const completed = this.#storage.sql.exec<Job>("SELECT * FROM managed_background_read_jobs WHERE id = ?", id).one();
    if (completed.state !== "complete") return;
    // At-least-once delivery: dedupe inside the callback, atomically with its durable message append.
    await this.#deliver({
      jobId: id, sessionId: completed.session_id, sourceTurnId: completed.turn_id ?? undefined,
      toolName: "web__run", trust: "untrusted_tool_result", content: completed.output ?? "null",
    });
    this.markDelivered(id);
    await this.#storage.sync();
  }
}

function bounded(value: string): string {
  return value.length > MAX_OUTPUT ? `${value.slice(0, MAX_OUTPUT)}\n[truncated background result]` : value;
}

function parseSearchInput(input: unknown): { search_query: { q: string }[] } {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("invalid background search input");
  const value = input as Record<string, unknown>;
  if (Object.keys(value).length !== 1 || !Array.isArray(value.search_query)
    || value.search_query.length < 1 || value.search_query.length > 4) throw new TypeError("search_query required (1–4 queries)");
  const queries = value.search_query.map((item: unknown) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) throw new TypeError("invalid search query");
    const record = item as Record<string, unknown>;
    if (Object.keys(record).length !== 1 || typeof record.q !== "string"
      || !record.q.trim() || record.q.length > 512) throw new TypeError("invalid search query");
    return { q: record.q };
  });
  if (JSON.stringify(queries).length > MAX_INPUT) throw new TypeError("background input too long");
  return { search_query: queries };
}
