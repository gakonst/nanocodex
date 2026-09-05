import { CronExpressionParser } from "cron-parser";

export const CRON_TRIGGER_ID = /^[A-Za-z0-9_-]{1,64}$/;
const MAX_TRIGGERS = 32;
const encoder = new TextEncoder();

export type CronTriggerConfig = {
  cron: string;
  timezone: string;
  input: string;
  enabled: boolean;
  session_mode: "new" | "continue";
};

export type CronTriggerRow = {
  id: string;
  revision: string;
  cron: string;
  timezone: string;
  input: string;
  enabled: number;
  session_mode: "new" | "continue";
  last_agent_id: string | null;
  authorization_json: string;
  authorization_epoch: number;
  request_hash: string;
  next_run_at: number | null;
  retry_at: number | null;
  last_run_at: number | null;
  last_turn_id: string | null;
  last_skipped_at: number | null;
  created_at: number;
  updated_at: number;
};

export function nextCronRun(cron: string, timezone: string, after: number): number {
  return CronExpressionParser.parse(cron, { tz: timezone, currentDate: after }).next().getTime();
}

export function parseCronTrigger(value: unknown, now: number, defaultMode: CronTriggerConfig["session_mode"] = "new"): CronTriggerConfig {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("cron trigger must be an object");
  }
  const body = value as Record<string, unknown>;
  if (Object.keys(body).some((key) => !["cron", "timezone", "input", "enabled", "session_mode"].includes(key))
    || typeof body.cron !== "string" || body.cron.length > 256
    || body.cron.trim().split(/\s+/).length !== 5
    // Do not accept random H fields: every occurrence must be deterministic.
    || /[^\d\s*,/\-A-Za-z]/.test(body.cron) || /H/i.test(body.cron.replace(/THU/gi, ""))
    || typeof body.input !== "string" || body.input.trim().length === 0
    || encoder.encode(body.input).byteLength > 64 * 1024
    || (body.session_mode !== undefined && body.session_mode !== "new" && body.session_mode !== "continue")
    || (body.enabled !== undefined && typeof body.enabled !== "boolean")
    || (body.timezone !== undefined && typeof body.timezone !== "string")) {
    throw new Error("expected a five-field cron, a non-empty input (at most 64 KiB), optional timezone, enabled and session_mode (new or continue)");
  }
  const timezone = body.timezone as string | undefined ?? "UTC";
  if (timezone.length > 128) throw new Error("invalid timezone");
  new Intl.DateTimeFormat("en", { timeZone: timezone });
  const config: CronTriggerConfig = {
    session_mode: body.session_mode as CronTriggerConfig["session_mode"] | undefined ?? defaultMode,
    cron: body.cron.trim().replace(/\s+/g, " "), timezone,
    input: body.input, enabled: body.enabled as boolean | undefined ?? true,
  };
  nextCronRun(config.cron, timezone, now);
  return config;
}

export function cronTriggerView(row: CronTriggerRow, agentId?: string) {
  return {
    id: row.id, cron: row.cron, timezone: row.timezone, input: row.input,
    session_mode: row.session_mode,
    last_agent_id: row.last_agent_id ?? (row.last_turn_id ? agentId ?? null : null),
    enabled: row.enabled === 1, next_run_at: row.next_run_at,
    last_run_at: row.last_run_at, last_turn_id: row.last_turn_id,
    last_skipped_at: row.last_skipped_at, created_at: row.created_at, updated_at: row.updated_at,
  };
}

export type CronDelivery = {
  id: string;
  trigger_id: string;
  trigger_created_at: number;
  agent_id: string;
  scheduled_at: number;
  payload_json: string;
  retry_at: number;
};

/** Session-owned schedules; dispatch advances this table in the turn-admission transaction. */
export class CronTriggers {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_cron_triggers (
      id TEXT PRIMARY KEY, revision TEXT NOT NULL, cron TEXT NOT NULL,
      timezone TEXT NOT NULL, input TEXT NOT NULL, enabled INTEGER NOT NULL,
      authorization_json TEXT NOT NULL, authorization_epoch INTEGER NOT NULL,
      request_hash TEXT NOT NULL, next_run_at INTEGER, retry_at INTEGER, last_run_at INTEGER,
      last_turn_id TEXT, last_skipped_at INTEGER,
      created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
    )`);
    // Rows written before execution modes existed must keep continuing their session.
    const columns = new Set(storage.sql.exec<{ name: string }>("PRAGMA table_info(managed_cron_triggers)").toArray().map((column) => column.name));
    if (!columns.has("session_mode")) storage.sql.exec("ALTER TABLE managed_cron_triggers ADD COLUMN session_mode TEXT NOT NULL DEFAULT 'continue'");
    if (!columns.has("last_agent_id")) storage.sql.exec("ALTER TABLE managed_cron_triggers ADD COLUMN last_agent_id TEXT");
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_cron_deliveries (
      id TEXT PRIMARY KEY, trigger_id TEXT NOT NULL UNIQUE, trigger_created_at INTEGER NOT NULL,
      agent_id TEXT NOT NULL, scheduled_at INTEGER NOT NULL, payload_json TEXT NOT NULL, retry_at INTEGER NOT NULL
    )`);
  }

  list(): CronTriggerRow[] {
    return this.storage.sql.exec<CronTriggerRow>("SELECT * FROM managed_cron_triggers ORDER BY id").toArray();
  }

  get(id: string): CronTriggerRow | undefined {
    return this.storage.sql.exec<CronTriggerRow>("SELECT * FROM managed_cron_triggers WHERE id = ?", id).toArray()[0];
  }

  put(id: string, config: CronTriggerConfig, authorization: string, epoch: number, hash: string, now: number): CronTriggerRow {
    const previous = this.get(id);
    if (!previous && this.list().length >= MAX_TRIGGERS) throw new Error("at most 32 cron triggers per agent");
    if (previous && previous.cron === config.cron && previous.timezone === config.timezone
      && previous.session_mode === config.session_mode && previous.input === config.input && previous.enabled === Number(config.enabled)
      && previous.authorization_json === authorization && previous.authorization_epoch === epoch) return previous;
    const next = config.enabled ? nextCronRun(config.cron, config.timezone, now) : null;
    this.storage.sql.exec(`INSERT INTO managed_cron_triggers (
      id, revision, cron, timezone, input, enabled, session_mode, authorization_json,
      authorization_epoch, request_hash, next_run_at, created_at, updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(id) DO UPDATE SET revision = excluded.revision, cron = excluded.cron,
      timezone = excluded.timezone, input = excluded.input, enabled = excluded.enabled, session_mode = excluded.session_mode,
      authorization_json = excluded.authorization_json, authorization_epoch = excluded.authorization_epoch,
      request_hash = excluded.request_hash, next_run_at = excluded.next_run_at, retry_at = NULL, updated_at = excluded.updated_at`,
    id, crypto.randomUUID(), config.cron, config.timezone, config.input, Number(config.enabled), config.session_mode,
    authorization, epoch, hash, next, now, now);
    return this.get(id)!;
  }

  delete(id: string): void {
    this.storage.sql.exec("DELETE FROM managed_cron_triggers WHERE id = ?", id);
  }

  nextAlarm(): number | undefined {
    return this.storage.sql.exec<{ next_run_at: number }>(
      `SELECT MIN(next_run_at) AS next_run_at FROM (
        SELECT MAX(next_run_at, COALESCE(retry_at, next_run_at)) AS next_run_at FROM managed_cron_triggers
        WHERE enabled = 1 AND id NOT IN (SELECT trigger_id FROM managed_cron_deliveries)
        UNION ALL SELECT retry_at FROM managed_cron_deliveries
      )`,
    ).toArray()[0]?.next_run_at ?? undefined;
  }

  due(now: number): CronTriggerRow[] {
    return this.storage.sql.exec<CronTriggerRow>(
      "SELECT * FROM managed_cron_triggers WHERE enabled = 1 AND next_run_at <= ? AND (retry_at IS NULL OR retry_at <= ?) AND id NOT IN (SELECT trigger_id FROM managed_cron_deliveries) ORDER BY next_run_at, id", now, now,
    ).toArray();
  }

  retry(row: CronTriggerRow, retryAt: number): void {
    this.storage.sql.exec("UPDATE managed_cron_triggers SET retry_at = ? WHERE id = ? AND revision = ? AND next_run_at = ?",
      retryAt, row.id, row.revision, row.next_run_at);
  }

  /** Call synchronously in the same transaction that inserts the accepted turn. */
  advance(row: CronTriggerRow, now: number, next: number, turnId?: string, agentId?: string): boolean {
    return this.storage.sql.exec(`UPDATE managed_cron_triggers SET next_run_at = ?, retry_at = NULL,
      last_run_at = CASE WHEN ? IS NOT NULL THEN ? ELSE last_run_at END,
      last_turn_id = COALESCE(?, last_turn_id), last_agent_id = COALESCE(?, last_agent_id),
      last_skipped_at = CASE WHEN ? IS NULL THEN ? ELSE last_skipped_at END
      WHERE id = ? AND revision = ? AND enabled = 1 AND next_run_at = ? RETURNING id`,
    next, turnId ?? null, row.next_run_at, turnId ?? null, agentId ?? null, turnId ?? null, now,
    row.id, row.revision, row.next_run_at).toArray().length === 1;
  }

  /** Claim a fresh-session occurrence and its replayable delivery in one transaction. */
  enqueue(row: CronTriggerRow, next: number, delivery: CronDelivery): boolean {
    return this.storage.transactionSync(() => {
      if (this.storage.sql.exec("SELECT id FROM managed_cron_deliveries WHERE trigger_id = ?", row.id).toArray().length) return false;
      const claimed = this.storage.sql.exec(`UPDATE managed_cron_triggers SET next_run_at = ?, retry_at = NULL
        WHERE id = ? AND revision = ? AND enabled = 1 AND next_run_at = ? RETURNING id`,
      next, row.id, row.revision, row.next_run_at).toArray().length;
      if (!claimed) return false;
      this.storage.sql.exec(`INSERT INTO managed_cron_deliveries
        (id, trigger_id, trigger_created_at, agent_id, scheduled_at, payload_json, retry_at) VALUES (?, ?, ?, ?, ?, ?, ?)`,
      delivery.id, delivery.trigger_id, delivery.trigger_created_at, delivery.agent_id, delivery.scheduled_at, delivery.payload_json, delivery.retry_at);
      return true;
    });
  }

  deliveries(now = Number.MAX_SAFE_INTEGER): CronDelivery[] {
    return this.storage.sql.exec<CronDelivery>("SELECT * FROM managed_cron_deliveries WHERE retry_at <= ? ORDER BY scheduled_at, id", now).toArray();
  }

  retryDelivery(id: string, at: number): void {
    this.storage.sql.exec("UPDATE managed_cron_deliveries SET retry_at = ? WHERE id = ?", at, id);
  }

  finishDelivery(delivery: CronDelivery, accepted: boolean): void {
    this.storage.transactionSync(() => {
      if (accepted) this.storage.sql.exec(`UPDATE managed_cron_triggers SET last_run_at = ?, last_turn_id = ?, last_agent_id = ?
        WHERE id = ? AND created_at = ? AND (last_run_at IS NULL OR last_run_at <= ?)`,
      delivery.scheduled_at, delivery.id, delivery.agent_id, delivery.trigger_id, delivery.trigger_created_at, delivery.scheduled_at);
      this.storage.sql.exec("DELETE FROM managed_cron_deliveries WHERE id = ?", delivery.id);
    });
  }
}
