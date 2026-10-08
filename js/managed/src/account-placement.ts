/**
 * Placement of each account's owner `AccountHostedTools` object.
 *
 * The legacy object is named by the bare owner id and stays wherever it was
 * first created. A home object is named by region and owner id and is created
 * with that region's location hint, next to the owner's Hands and threads.
 *
 * Exactly one object per owner is active. A moved object is a fail-closed
 * tombstone naming its successor; a migrating object refuses everything while
 * its export is being adopted. Refusals happen before any work, so routed
 * callers may re-locate and retry. Regional relays never move.
 */
import type { AccountHostedTools } from "./account-hosted-tools";

export const ACCOUNT_PLACEMENT_KEY = "account_placement_v1";
/** Migration ids a home durably refused; their source may safely resume. */
export const ACCOUNT_REJECTED_KEY = "account_placement_rejected_v1";
/** Alarm carried by an adopted export, armed by the next instance. */
export const ACCOUNT_ADOPTED_ALARM_KEY = "account_placement_alarm_v1";
const PLACEMENT_KEYS = new Set(["account_placement_v1", "account_placement_rejected_v1", "account_placement_alarm_v1"]);
export const ACCOUNT_PLACEMENT_REGIONS: ReadonlySet<string> = new Set(["wnam", "enam", "sam", "weur", "eeur", "apac", "oc", "afr", "me"]);
/** "~" and "/" are outside the owner-id alphabet, so names never collide. */
export const HOME_ACCOUNT_PREFIX = "~home/v1/";
export const ACCOUNT_MOVED = "account_moved";
export const ACCOUNT_MIGRATING = "account_migrating";
export const ACCOUNT_MOVED_HEADER = "x-nanocodex-account-moved";
export const ACCOUNT_MIGRATING_HEADER = "x-nanocodex-account-migrating";
/** Minimum residency of an object before another re-home. */
export const ACCOUNT_REHOME_COOLDOWN_MS = 15 * 60_000;
const MAX_HOPS = 8;
const MIGRATING_RETRIES = 20;
const MIGRATING_DELAY_MS = 150;
const OWNER_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;

export type AccountName =
  | Readonly<{ kind: "legacy"; owner: string; name: string }>
  | Readonly<{ kind: "home"; owner: string; region: DurableObjectLocationHint; name: string }>;

export function homeAccountName(region: string, owner: string): string {
  if (!ACCOUNT_PLACEMENT_REGIONS.has(region) || !OWNER_ID.test(owner)) throw new TypeError("invalid account placement");
  return `${HOME_ACCOUNT_PREFIX}${region}/${owner}`;
}

export function parseAccountName(name: unknown): AccountName | undefined {
  if (typeof name !== "string") return undefined;
  if (OWNER_ID.test(name)) return { kind: "legacy", owner: name, name };
  if (!name.startsWith(HOME_ACCOUNT_PREFIX)) return undefined;
  const rest = name.slice(HOME_ACCOUNT_PREFIX.length);
  const slash = rest.indexOf("/");
  if (slash < 0) return undefined;
  const region = rest.slice(0, slash), owner = rest.slice(slash + 1);
  if (!ACCOUNT_PLACEMENT_REGIONS.has(region) || !OWNER_ID.test(owner)) return undefined;
  return { kind: "home", owner, region: region as DurableObjectLocationHint, name };
}

export type AccountPlacement =
  | Readonly<{ state: "active"; name?: string; activated_at?: number; migration_id?: string; source?: string }>
  | Readonly<{ state: "migrating"; target: string; migration_id: string; started_at: number; source: string }>
  | Readonly<{ state: "moved"; target: string; moved_at: number }>;

export function validPlacement(value: unknown): AccountPlacement | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const row = value as Record<string, unknown>;
  if (row.state === "active") return {
    state: "active",
    ...(typeof row.name === "string" && parseAccountName(row.name) ? { name: row.name } : {}),
    ...(typeof row.activated_at === "number" ? { activated_at: row.activated_at } : {}),
    ...(typeof row.migration_id === "string" ? { migration_id: row.migration_id } : {}),
    ...(typeof row.source === "string" ? { source: row.source } : {}) };
  if (row.state === "migrating" && parseAccountName(row.target) && typeof row.migration_id === "string"
    && typeof row.started_at === "number" && parseAccountName(row.source))
    return { state: "migrating", target: row.target as string, migration_id: row.migration_id, started_at: row.started_at, source: row.source as string };
  if (row.state === "moved" && parseAccountName(row.target) && typeof row.moved_at === "number")
    return { state: "moved", target: row.target as string, moved_at: row.moved_at };
  return undefined;
}

/** A placement refusal carried by an RPC error message, or undefined. */
export function placementRefusal(error: unknown): { kind: "moved"; target?: string } | { kind: "migrating" } | undefined {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : undefined;
  if (!message) return undefined;
  if (message.includes(ACCOUNT_MIGRATING)) return { kind: "migrating" };
  const index = message.indexOf(ACCOUNT_MOVED);
  if (index < 0) return undefined;
  const rest = message.slice(index + ACCOUNT_MOVED.length);
  const target = rest.startsWith(":") ? rest.slice(1).trim().split(/\s/)[0] : undefined;
  return { kind: "moved", ...(target && parseAccountName(target) ? { target } : {}) };
}

/** Durable account export: every owner table except diagnostics, plus KV. */
export type AccountExport = Readonly<{
  migration_id: string;
  source: string;
  target: string;
  owner: string;
  tables: readonly Readonly<{ name: string; sql: string; columns: readonly string[]; rows: readonly unknown[][] }>[];
  indexes: readonly string[];
  kv: readonly [string, unknown][];
  alarm: number | null;
}>;

const EXCLUDED_TABLE = /^(sqlite_|_cf_|diagnostic_)/;

export function exportAccountStorage(storage: DurableObjectStorage, header: Pick<AccountExport, "migration_id" | "source" | "target" | "owner">, alarm: number | null): AccountExport {
  const objects = storage.sql.exec<{ type: string; name: string; sql: string | null }>(
    "SELECT type,name,sql FROM sqlite_master WHERE type IN ('table','index') ORDER BY rowid").toArray();
  const tables = objects.filter(entry => entry.type === "table" && entry.sql && !EXCLUDED_TABLE.test(entry.name)).map(entry => {
    const cursor = storage.sql.exec(`SELECT * FROM "${entry.name.replaceAll('"', '""')}"`);
    const columns = cursor.columnNames;
    const rows = [...cursor.raw()].map(row => row.map(value => value instanceof ArrayBuffer ? { $bytes: [...new Uint8Array(value)] } : value));
    return { name: entry.name, sql: entry.sql!, columns, rows };
  });
  const indexes = objects.filter(entry => entry.type === "index" && entry.sql && !EXCLUDED_TABLE.test(entry.name)).map(entry => entry.sql!);
  const kv = [...storage.kv.list()].filter(([key]) => !PLACEMENT_KEYS.has(key)) as [string, unknown][];
  return { ...header, tables, indexes, kv, alarm };
}

/** Replaces this object's state with an export. Must run inside transactionSync. */
export function importAccountStorage(storage: DurableObjectStorage, data: AccountExport): void {
  const quote = (name: string) => `"${name.replaceAll('"', '""')}"`;
  for (const entry of storage.sql.exec<{ name: string }>("SELECT name FROM sqlite_master WHERE type='table'").toArray()) {
    if (!EXCLUDED_TABLE.test(entry.name)) storage.sql.exec(`DROP TABLE IF EXISTS ${quote(entry.name)}`);
  }
  for (const [key] of [...storage.kv.list()]) if (key !== ACCOUNT_REJECTED_KEY) storage.kv.delete(key);
  for (const table of data.tables) {
    if (EXCLUDED_TABLE.test(table.name) || !/^CREATE TABLE/i.test(table.sql)) throw new Error("invalid account export");
    storage.sql.exec(table.sql);
    if (!table.rows.length) continue;
    const statement = `INSERT INTO ${quote(table.name)} (${table.columns.map(quote).join(",")}) VALUES (${table.columns.map(() => "?").join(",")})`;
    for (const row of table.rows) {
      storage.sql.exec(statement, ...row.map(value => value && typeof value === "object" && Array.isArray((value as { $bytes?: unknown }).$bytes)
        ? new Uint8Array((value as { $bytes: number[] }).$bytes).buffer : value as SqlStorageValue));
    }
  }
  for (const sql of data.indexes) if (/^CREATE (UNIQUE )?INDEX/i.test(sql)) storage.sql.exec(sql.replace(/^CREATE (UNIQUE )?INDEX (IF NOT EXISTS )?/i, (_, unique) => `CREATE ${unique ?? ""}INDEX IF NOT EXISTS `));
  for (const [key, value] of data.kv) if (!PLACEMENT_KEYS.has(key)) storage.kv.put(key, value);
}

export type AccountToolsNamespace = Readonly<{ getByName(owner: string): DurableObjectStub<AccountHostedTools> }>;
type RawNamespace = DurableObjectNamespace<AccountHostedTools>;

/** Raw stub for an exact account object name (legacy or home). */
export function accountObjectStub(namespace: RawNamespace, name: string): DurableObjectStub<AccountHostedTools> {
  const parsed = parseAccountName(name);
  return parsed?.kind === "home"
    ? namespace.get(namespace.idFromName(name), { locationHint: parsed.region })
    : namespace.getByName(name);
}

const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

/**
 * Owner-addressed account routing. Each isolate remembers where an owner's
 * account lives and follows moved tombstones; refusals happen before any
 * work, so a single re-located retry is safe. Bodies are buffered for it.
 */
class RoutedAccountTools implements AccountToolsNamespace {
  readonly #names = new Map<string, string>();
  constructor(readonly raw: RawNamespace) {}

  getByName(owner: string): DurableObjectStub<AccountHostedTools> {
    if (parseAccountName(owner)?.kind !== "legacy") return this.raw.getByName(owner);
    const routed = this;
    return new Proxy(Object.create(null), {
      get(_target, property) {
        if (property === "then" || typeof property === "symbol") return undefined;
        if (property === "fetch") return (input: RequestInfo | URL, init?: RequestInit) => routed.#fetch(owner, input, init);
        return (...args: unknown[]) => routed.#rpc(owner, property, args);
      },
    }) as DurableObjectStub<AccountHostedTools>;
  }

  /** Test/diagnostic view of this isolate's routing. */
  current(owner: string): string { return this.#names.get(owner) ?? owner; }

  #follow(owner: string, target: string | undefined): void {
    const parsed = target === undefined ? undefined : parseAccountName(target);
    if (parsed && parsed.owner === owner) this.#names.set(owner, target!);
    else this.#names.delete(owner);
  }

  async #fetch(owner: string, input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    const request = new Request(input, init);
    const body = request.body ? await request.arrayBuffer() : undefined;
    let hops = 0, waits = 0;
    while (true) {
      const name = this.current(owner);
      const response = await accountObjectStub(this.raw, name).fetch(new Request(request.url, {
        method: request.method, headers: request.headers, signal: request.signal, ...(body === undefined ? {} : { body }) }));
      if (response.status === 421 && response.headers.has(ACCOUNT_MOVED_HEADER) && hops++ < MAX_HOPS) {
        await response.body?.cancel().catch(() => undefined);
        this.#follow(owner, response.headers.get(ACCOUNT_MOVED_HEADER) || undefined);
        continue;
      }
      if (response.status === 503 && response.headers.has(ACCOUNT_MIGRATING_HEADER) && waits++ < MIGRATING_RETRIES && !request.signal.aborted) {
        await response.body?.cancel().catch(() => undefined);
        await delay(MIGRATING_DELAY_MS);
        continue;
      }
      return response;
    }
  }

  async #rpc(owner: string, method: string, args: unknown[]): Promise<unknown> {
    let hops = 0, waits = 0;
    while (true) {
      const stub = accountObjectStub(this.raw, this.current(owner)) as unknown as Record<string, (...values: unknown[]) => Promise<unknown>>;
      try {
        return await stub[method]!(...args);
      } catch (error) {
        const refusal = placementRefusal(error);
        if (refusal?.kind === "moved" && hops++ < MAX_HOPS) { this.#follow(owner, refusal.target); continue; }
        if (refusal?.kind === "migrating" && waits++ < MIGRATING_RETRIES) { await delay(MIGRATING_DELAY_MS); continue; }
        throw error;
      }
    }
  }
}

const routers = new WeakMap<RawNamespace, RoutedAccountTools>();

/** Owner-routed account namespace. Callers check the binding first. */
export function accountTools(env: { NANOCODEX_ACCOUNT_TOOLS?: RawNamespace }): AccountToolsNamespace {
  const raw = env.NANOCODEX_ACCOUNT_TOOLS;
  if (!raw) throw new Error("account tools binding is unavailable");
  let routed = routers.get(raw);
  if (!routed) { routed = new RoutedAccountTools(raw); routers.set(raw, routed); }
  return routed;
}

/** Where this isolate currently routes an owner (diagnostics and tests). */
export function routedAccountName(env: { NANOCODEX_ACCOUNT_TOOLS?: RawNamespace }, owner: string): string {
  const raw = env.NANOCODEX_ACCOUNT_TOOLS;
  return raw ? routers.get(raw)?.current(owner) ?? owner : owner;
}
