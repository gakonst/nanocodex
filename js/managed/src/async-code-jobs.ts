import { inputChunks } from "./managed-turn-input";

/** Durable ownership and delivery outbox for opt-in Code Mode jobs.
 * JavaScript is never re-executed after an uncertain dispatch. Native nested
 * effect receipts remain available for reconciliation through the usual journal.
 */
export type CodeJobContext = {
  sessionId: string; parentCallId: string; turnId?: string; operationId?: string; modelCallIndex?: number; model: string;
  source: string; cellId: string; maxOutputTokens: number;
};
export type CodeJobOwner = { turnId: string; authorization: string; epoch: number };
export type CodeJob = {
  id: string; runtime_session_id: string; runtime_turn_id: string; operation_id: string; model_call_index: number; source_call_id: string; turn_id: string;
  authorization_json: string; authorization_epoch: number; source_hash: string; generation: string;
  state: "running" | "terminal" | "delivered" | "cancelled"; result_json: string | null;
  delivery_turn_id: string | null; created_at: number; updated_at: number;
};
type Storage = Pick<DurableObjectStorage, "sql" | "transactionSync">;
const MAX_JOBS = 4096;
const MAX_PENDING = 32;
const MAX_RESULT_BYTES = 8 * 1024 * 1024;
const MAX_RETAINED_BYTES = 33554432;
const interrupted = JSON.stringify({ success: false, output: "Code Mode job was interrupted before a terminal receipt was saved. Dispatched effects may have completed. Reconcile their original operation IDs before retrying.", code: "ASYNC_JOB_INTERRUPTED", outcome: "unknown", nested_calls: [], notifications: [] });

export class AsyncCodeJobs {
  constructor(private readonly storage: Storage, private readonly generation: string) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_async_code_jobs (
      id TEXT PRIMARY KEY, runtime_session_id TEXT NOT NULL, runtime_turn_id TEXT NOT NULL, source_call_id TEXT NOT NULL,
      operation_id TEXT NOT NULL, model_call_index INTEGER NOT NULL, turn_id TEXT NOT NULL, authorization_json TEXT NOT NULL, authorization_epoch INTEGER NOT NULL, source_hash TEXT NOT NULL,
      generation TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('running','terminal','delivered','cancelled')),
      result_json TEXT, delivery_turn_id TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
      UNIQUE(runtime_session_id, operation_id, model_call_index, source_call_id))`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_async_code_results (
      job_id TEXT NOT NULL, chunk_index INTEGER NOT NULL, value TEXT NOT NULL,
      PRIMARY KEY(job_id, chunk_index))`);
    storage.sql.exec(`CREATE INDEX IF NOT EXISTS managed_async_code_delivery ON managed_async_code_jobs(state, created_at)`);
    // A new isolate cannot resume an arbitrary JS continuation. Retain its job
    // identity and dispatch fence, and deliver an explicit uncertain outcome.
    storage.sql.exec(`UPDATE managed_async_code_jobs SET state='terminal', result_json=?, updated_at=?
      WHERE state='running' AND generation<>?`, interrupted, Date.now(), generation);
  }

  async admit(context: CodeJobContext, owner: CodeJobOwner): Promise<{ jobId: string; status: "execute" | "existing" }> {
    if (!context.sessionId || !context.parentCallId || !context.turnId || !context.operationId || !Number.isSafeInteger(context.modelCallIndex) || context.modelCallIndex! < 0 || !owner.turnId || owner.authorization.length > 65536)
      throw new Error("invalid async job ownership");
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify([context.source, context.maxOutputTokens])));
    const hash = Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, "0")).join("");
    return this.storage.transactionSync(() => {
      const previous = this.storage.sql.exec<CodeJob>(`SELECT * FROM managed_async_code_jobs
        WHERE runtime_session_id=? AND operation_id=? AND model_call_index=? AND source_call_id=?`, context.sessionId, context.operationId!, context.modelCallIndex!, context.parentCallId).toArray()[0];
      if (previous) {
        if (previous.turn_id !== owner.turnId || previous.source_hash !== hash || previous.authorization_json !== owner.authorization || previous.authorization_epoch !== owner.epoch)
          throw new Error("async job identity conflict");
        return { jobId: previous.id, status: "existing" as const };
      }
      const counts = this.storage.sql.exec<{ total: number; pending: number; bytes: number }>(`SELECT COUNT(*) AS total,
        COALESCE(SUM(CASE WHEN state IN ('running','terminal') THEN 1 ELSE 0 END),0) AS pending,
        COALESCE(SUM(length(CAST(result_json AS BLOB))),0) + (SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM managed_async_code_results) AS bytes FROM managed_async_code_jobs`).one();
      if (counts.total >= MAX_JOBS || counts.pending >= MAX_PENDING || counts.bytes >= MAX_RETAINED_BYTES)
        throw new Error("async Code Mode job capacity reached; finish pending jobs or start a new thread");
      const id = crypto.randomUUID(), now = Date.now();
      this.storage.sql.exec(`INSERT INTO managed_async_code_jobs
        (id,runtime_session_id,runtime_turn_id,source_call_id,operation_id,model_call_index,turn_id,authorization_json,authorization_epoch,source_hash,generation,state,created_at,updated_at)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,'running',?,?)`, id, context.sessionId, context.turnId, context.parentCallId, context.operationId!, context.modelCallIndex!, owner.turnId,
        owner.authorization, owner.epoch, hash, this.generation, now, now);
      return { jobId: id, status: "execute" as const };
    });
  }

  complete(id: string, receipt: unknown): void {
    let value = JSON.stringify(receipt);
    if (value === undefined) throw new Error("missing async Code Mode receipt");
    if (new TextEncoder().encode(value).byteLength > MAX_RESULT_BYTES) {
      value = JSON.stringify({ success: false, code: "ASYNC_OUTPUT_TOO_LARGE", output: "The job finished, but its output exceeded the retained result limit. Effects must not be repeated merely to recover output.", nested_calls: [], notifications: [] });
    }
    this.storage.transactionSync(() => {
      const row = this.get(id);
      if (!row) throw new Error("unknown async Code Mode job");
      if (row.generation !== this.generation) throw new Error("retired async Code Mode generation");
      if (row.state === "delivered" || row.state === "cancelled") return;
      if (row.state !== "running") {
        if (row.result_json !== value) throw new Error("async job terminal receipt conflict");
        return;
      }
      const retainedBytes = this.storage.sql.exec<{bytes: number}>(`SELECT
        (SELECT COALESCE(SUM(length(CAST(result_json AS BLOB))),0) FROM managed_async_code_jobs) +
        (SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM managed_async_code_results) AS bytes`).one().bytes;
      if (retainedBytes + new TextEncoder().encode(value).byteLength > MAX_RETAINED_BYTES - MAX_JOBS * 512) {
        value = JSON.stringify({ success: false, code: "ASYNC_STORAGE_FULL", output: "The job finished, but retained receipt storage is full. Effects must not be repeated merely to recover output.", nested_calls: [], notifications: [] });
      }
      let index = 0;
      for (const chunk of inputChunks(value)) {
        this.storage.sql.exec("INSERT INTO managed_async_code_results(job_id,chunk_index,value) VALUES(?,?,?)", id, index++, chunk);
      }
      this.storage.sql.exec(`UPDATE managed_async_code_jobs SET state='terminal',result_json=NULL,updated_at=? WHERE id=? AND state='running'`, Date.now(), id);
    });
  }

  forCall(sessionId: string, callId: string, turnId?: string): CodeJob | undefined {
    const rows = turnId === undefined
      ? this.storage.sql.exec<CodeJob>("SELECT * FROM managed_async_code_jobs WHERE runtime_session_id=? AND source_call_id=? LIMIT 2", sessionId, callId).toArray()
      : this.storage.sql.exec<CodeJob>("SELECT * FROM managed_async_code_jobs WHERE runtime_session_id=? AND source_call_id=? AND runtime_turn_id=?", sessionId, callId, turnId).toArray();
    if (rows.length > 1) throw new Error("ambiguous async Code Mode origin");
    return rows[0];
  }
  forDelivery(turnId: string): CodeJob | undefined {
    const row = this.storage.sql.exec<CodeJob>("SELECT * FROM managed_async_code_jobs WHERE delivery_turn_id=? AND id=substr(?,7)", turnId, turnId).toArray()[0];
    return row ? this.hydrate(row) : undefined;
  }
  private hydrate(row: CodeJob): CodeJob {
    if (row.result_json === null) {
      const chunks = this.storage.sql.exec<{value: string}>("SELECT value FROM managed_async_code_results WHERE job_id=? ORDER BY chunk_index", row.id).toArray();
      if (chunks.length) row.result_json = chunks.map(chunk => chunk.value).join("");
    }
    return row;
  }
  cancelDelivery(id: string, turnId: string): void {
    this.storage.sql.exec("UPDATE managed_async_code_jobs SET state='cancelled',updated_at=? WHERE id=? AND state='terminal' AND delivery_turn_id=?", Date.now(), id, turnId);
  }
  cancelAll(): void {
    this.storage.sql.exec("UPDATE managed_async_code_jobs SET state='cancelled',updated_at=? WHERE state IN ('running','terminal')", Date.now());
  }
  cancelTurn(turnId: string): void {
    this.storage.sql.exec("UPDATE managed_async_code_jobs SET state='cancelled',updated_at=? WHERE turn_id=? AND state IN ('running','terminal')", Date.now(), turnId);
  }
  get(id: string): CodeJob | undefined {
    const row = this.storage.sql.exec<CodeJob>("SELECT * FROM managed_async_code_jobs WHERE id=?", id).toArray()[0];
    return row ? this.hydrate(row) : undefined;
  }
  pending(): CodeJob[] {
    return this.storage.sql.exec<CodeJob>("SELECT * FROM managed_async_code_jobs WHERE state='terminal' ORDER BY created_at,id LIMIT 32").toArray().map(row => this.hydrate(row));
  }
  hasRunning(): boolean {
    return this.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_async_code_jobs WHERE state='running'").one().n > 0;
  }
  hasPending(): boolean { return this.pending().length > 0; }
  hasPendingContinuation(): boolean {
    return this.storage.sql.exec<{ n: number }>(`SELECT COUNT(*) AS n FROM managed_async_code_jobs j
      JOIN managed_turns t ON t.id=j.delivery_turn_id
      WHERE j.state='delivered' AND t.id='async:'||j.id AND t.state IN ('accepted','cancelling')`).one().n > 0;
  }
  bindDelivery(id: string, turnId: string): void {
    this.storage.sql.exec("UPDATE managed_async_code_jobs SET delivery_turn_id=?,updated_at=? WHERE id=? AND state='terminal' AND delivery_turn_id IS NULL", turnId, Date.now(), id);
    if (this.get(id)?.delivery_turn_id !== turnId) throw new Error("async job delivery target conflict");
  }
  rebindDelivery(id: string, previous: string, next: string): void {
    this.storage.sql.exec("UPDATE managed_async_code_jobs SET delivery_turn_id=?,updated_at=? WHERE id=? AND state='terminal' AND delivery_turn_id=?", next, Date.now(), id, previous);
    if (this.get(id)?.delivery_turn_id !== next) throw new Error("async job delivery target conflict");
  }
  delivered(id: string, turnId: string): void {
    this.storage.sql.exec("UPDATE managed_async_code_jobs SET state='delivered',updated_at=? WHERE id=? AND state='terminal' AND delivery_turn_id=?", Date.now(), id, turnId);
    // Keep identity tombstones. Payload pruning cannot make an old call executable.
    this.storage.sql.exec("DELETE FROM managed_async_code_results WHERE job_id IN (SELECT id FROM managed_async_code_jobs WHERE state='delivered' AND updated_at<? AND NOT EXISTS (SELECT 1 FROM managed_turns t WHERE t.id=managed_async_code_jobs.delivery_turn_id AND t.state IN ('accepted','cancelling')))", Date.now() - 7 * 86400000);
    this.storage.sql.exec("UPDATE managed_async_code_jobs SET result_json=NULL WHERE state='delivered' AND updated_at<? AND NOT EXISTS (SELECT 1 FROM managed_turns t WHERE t.id=managed_async_code_jobs.delivery_turn_id AND t.state IN ('accepted','cancelling'))", Date.now() - 7 * 86400000);
  }
}
