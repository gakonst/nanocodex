import { AsyncLocalStorage } from "node:async_hooks";

const MAX_RECORDS = 20_000;
const RETENTION_MS = 7 * 24 * 60 * 60 * 1_000;
const ID = /^[A-Za-z0-9_./:-]{1,160}$/;
const STRING_FIELDS = new Set([
  "type", "stage", "message_type", "operation", "tool", "outcome", "reason_code", "host_stage",
  "thread_id", "session_id", "runtime_session_id", "request_id", "turn_id", "managed_turn_id",
  "runtime_turn_id", "started_runtime_turn_id", "host_turn_id", "tool_call_id", "source_call_id",
  "transport_call_id", "parent_call_id", "socket_id", "egress_request_id", "relay_id",
  "provider_request_id", "response_id", "lease_id", "connection_id", "host_connection_id", "host_runtime_id",
  "phase", "operation_kind", "transport", "error_kind", "error_code", "failure_phase", "replay_mode",
  "hand_id", "remote_generation", "role", "provider_event_type", "output_kind",
]);
const NUMBER_FIELDS = new Set([
  "duration_ms", "elapsed_ms", "host_elapsed_ms", "send_wait_ms", "first_message_ms", "first_output_ms", "first_reasoning_delta_ms", "first_answer_delta_ms", "first_tool_delta_ms",
  "last_message_age_ms", "socket_queue_residence_max_ms", "pre_inference_ms", "engine_queue_max_ms",
  "engine_service_ttft_total_ms", "admission_ms", "roundtrip_ms", "settlement_ms", "transit_return_overhead_ms",
  "time_to_first_event_ms", "time_to_first_output_ms", "started_after_ms", "fetch_ms", "decode_ms", "total_ms",
  "input_decode_ms", "ownership_ms", "resolve_ms", "handler_ms", "lease_expires_at", "last_heartbeat_at", "heartbeat_age_ms",
  "event_seq", "agent_id", "connection_generation", "runtime_generation", "socket_request_index", "model_call_index",
  "received_message_count", "queued_message_count", "socket_delivered_message_count", "buffered_send_bytes",
  "pending_calls", "pending_call_count", "heartbeat_count", "close_code", "attempt_count", "attempt", "retry_delay_ms",
  "next_attempt", "max_attempts", "status_code",
  "renewal_count", "dispatch_to_message_ms", "frame_decode_ms", "lease_validation_ms", "message_to_handler_ms",
]);
const BOOLEAN_FIELDS = new Set(["success", "intentional", "close_clean", "replayed", "start_observed", "reconnect_enabled", "opens_new_socket", "server_requested_delay", "active", "connected"]);
// Only broker schema names survive persistence; arbitrary strings/values do not.
const CONFLICT_FIELDS = new Set([
  "call_id", "session_id", "source_call_id", "turn_id", "host_id", "lease_id",
  "generation", "model", "name", "input_json", "output_token_budget",
  "output_byte_budget", "deadline_at",
]);
const HOST_TIMING_FIELDS = new Set(["scheduler_ms", "execution_gate_ms", "execution_ms", "result_encode_ms", "result_queue_ms", "host_elapsed_ms"]);

export type DiagnosticPage = Readonly<{
  service: string;
  available: boolean;
  retention_ms: number;
  max_records: number;
  oldest_seq?: number;
  latest_seq?: number;
  next_after: number;
  history_truncated: boolean;
  write_failed?: boolean;
  events: readonly Record<string, unknown>[];
}>;

/** Content-free boundary evidence, independent of native invocation lifetime.
 * SQL failures never change the observed operation. Readers see availability
 * and retention explicitly rather than treating an empty page as proof.
 */
export class DiagnosticJournal {
  #initialized = false;
  #writeFailed = false;
  constructor(readonly storage: DurableObjectStorage, readonly service: "managed" | "hand.broker") {}

  record(observation: unknown): void {
    try {
      const safe = project(observation);
      if (!safe || typeof safe.type !== "string") return;
      this.#initialize();
      const row = this.storage.sql.exec<{ seq: number }>(
        "INSERT INTO diagnostic_events(created_at, thread_id, lease_id, connection_id, payload_json) VALUES(?,?,?,?,?) RETURNING seq",
        Date.now(), safe.thread_id ?? null, safe.lease_id ?? null, safe.connection_id ?? null, JSON.stringify(safe),
      ).one();
      if (row.seq % 64 !== 0) return;
      const pruned = this.storage.sql.exec<{ seq: number }>(
        "DELETE FROM diagnostic_events WHERE seq <= ? OR created_at < ? RETURNING seq", row.seq - MAX_RECORDS, Date.now() - RETENTION_MS,
      ).toArray();
      if (pruned.length) this.storage.sql.exec(
        "UPDATE diagnostic_retention SET pruned_through = MAX(pruned_through, ?) WHERE singleton=1", Math.max(...pruned.map(row => row.seq)),
      );
    } catch {
      this.#writeFailed = true;
      // Record the gap if storage recovers. Total storage/process loss can
      // prevent even this marker; it must never fail or retry the real effect.
      try {
        this.#initialize();
        this.storage.sql.exec("UPDATE diagnostic_retention SET write_failed=1 WHERE singleton=1");
      } catch { /* Storage is still unavailable. */ }
    }
  }

  page(threadId: string, after: number, limit: number, includeConnections = false): DiagnosticPage {
    const base = { service: this.service, retention_ms: RETENTION_MS, max_records: MAX_RECORDS + 63, next_after: after };
    try {
      this.#initialize();
      const expired = this.storage.sql.exec<{ seq: number }>(
        "DELETE FROM diagnostic_events WHERE created_at < ? RETURNING seq", Date.now() - RETENTION_MS,
      ).toArray();
      if (expired.length) this.storage.sql.exec(
        "UPDATE diagnostic_retention SET pruned_through = MAX(pruned_through, ?) WHERE singleton=1", Math.max(...expired.map(row => row.seq)),
      );
      const bounds = this.storage.sql.exec<{ oldest: number | null; latest: number | null }>(
        "SELECT MIN(seq) AS oldest, MAX(seq) AS latest FROM diagnostic_events",
      ).one();
      if (this.#writeFailed) this.storage.sql.exec("UPDATE diagnostic_retention SET write_failed=1 WHERE singleton=1");
      const retention = this.storage.sql.exec<{ pruned_through: number; write_failed: number }>("SELECT pruned_through,write_failed FROM diagnostic_retention WHERE singleton=1").one();
      const related = includeConnections ? ` OR (thread_id IS NULL AND (
        lease_id IN (SELECT DISTINCT lease_id FROM diagnostic_events WHERE thread_id=? AND lease_id IS NOT NULL)
        OR connection_id IN (SELECT DISTINCT connection_id FROM diagnostic_events WHERE thread_id=? AND connection_id IS NOT NULL)))` : "";
      const rows = this.storage.sql.exec<{ seq: number; created_at: number; payload_json: string }>(
        `SELECT seq,created_at,payload_json FROM diagnostic_events WHERE seq > ? AND (thread_id=?${related}) ORDER BY seq LIMIT ?`,
        after, threadId, ...(includeConnections ? [threadId, threadId] : []), limit,
      ).toArray();
      return { ...base, available: true, ...(bounds.oldest === null ? {} : { oldest_seq: bounds.oldest, latest_seq: bounds.latest! }),
        next_after: rows.at(-1)?.seq ?? after, history_truncated: retention.pruned_through > after || retention.write_failed !== 0,
        write_failed: retention.write_failed !== 0,
        events: rows.map(row => ({ ...JSON.parse(row.payload_json), seq: row.seq, created_at: row.created_at, service: this.service })),
      };
    } catch { return { ...base, available: false, history_truncated: true, events: [] }; }
  }

  #initialize(): void {
    if (this.#initialized) return;
    this.storage.sql.exec(`CREATE TABLE IF NOT EXISTS diagnostic_events (
      seq INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, thread_id TEXT,
      lease_id TEXT, connection_id TEXT, payload_json TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS diagnostic_events_thread ON diagnostic_events(thread_id,seq);
      DROP INDEX IF EXISTS diagnostic_events_lease;
      DROP INDEX IF EXISTS diagnostic_events_connection;
      CREATE INDEX IF NOT EXISTS diagnostic_events_lease_present ON diagnostic_events(lease_id,seq) WHERE lease_id IS NOT NULL;
      CREATE INDEX IF NOT EXISTS diagnostic_events_connection_present ON diagnostic_events(connection_id,seq) WHERE connection_id IS NOT NULL;
      CREATE TABLE IF NOT EXISTS diagnostic_retention(singleton INTEGER PRIMARY KEY CHECK(singleton=1),pruned_through INTEGER NOT NULL,write_failed INTEGER NOT NULL DEFAULT 0);
      INSERT OR IGNORE INTO diagnostic_retention(singleton,pruned_through) VALUES(1,0);`);
    const columns = this.storage.sql.exec<{ name: string }>("PRAGMA table_info(diagnostic_retention)").toArray();
    if (!columns.some(column => column.name === "write_failed"))
      this.storage.sql.exec("ALTER TABLE diagnostic_retention ADD COLUMN write_failed INTEGER NOT NULL DEFAULT 0");
    this.#initialized = true;
  }
}

const scope = new AsyncLocalStorage<DiagnosticJournal>();
export function diagnosticScope<T>(journal: DiagnosticJournal, run: () => T): T { return scope.run(journal, run); }
export function recordDiagnostic(observation: unknown): void { scope.getStore()?.record(observation); }

export function diagnosticQuery(url: URL): { managedAfter: number; handAfter: number; limit: number } | undefined {
  const allowed = new Set(["after_managed", "after_hand", "limit", "public_origin"]);
  if ([...url.searchParams.keys()].some(key => !allowed.has(key) || url.searchParams.getAll(key).length !== 1)) return;
  const integer = (key: string, fallback: number) => {
    const raw = url.searchParams.get(key);
    if (raw === null) return fallback;
    if (!/^(0|[1-9][0-9]*)$/.test(raw)) return;
    const value = Number(raw); return Number.isSafeInteger(value) ? value : undefined;
  };
  const managedAfter = integer("after_managed", 0), handAfter = integer("after_hand", 0), limit = integer("limit", 256);
  if (managedAfter === undefined || handAfter === undefined || limit === undefined || limit < 1 || limit > 1_024) return;
  return { managedAfter, handAfter, limit };
}

function project(value: unknown): Record<string, unknown> | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  const safe: Record<string, unknown> = {};
  for (const [key, field] of Object.entries(value)) {
    if (STRING_FIELDS.has(key) && typeof field === "string" && ID.test(field)) safe[key] = field;
    if (NUMBER_FIELDS.has(key) && typeof field === "number" && Number.isFinite(field) && Math.abs(field) <= Number.MAX_SAFE_INTEGER) safe[key] = field;
    if (BOOLEAN_FIELDS.has(key) && typeof field === "boolean") safe[key] = field;
    if (key === "conflict_fields" && Array.isArray(field)) safe[key] = [...new Set(
      field.slice(0, CONFLICT_FIELDS.size).filter(name => typeof name === "string" && CONFLICT_FIELDS.has(name)),
    )];
    if (key === "host_timing" && field && typeof field === "object" && !Array.isArray(field)) safe[key] = Object.fromEntries(
      Object.entries(field).filter(([name, measurement]) => HOST_TIMING_FIELDS.has(name)
        && typeof measurement === "number" && Number.isFinite(measurement) && measurement >= 0 && measurement <= Number.MAX_SAFE_INTEGER),
    );
  }
  return safe;
}
