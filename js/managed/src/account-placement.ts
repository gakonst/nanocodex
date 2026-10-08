/**
 * Placement of each account's owner `AccountHostedTools` object.
 *
 * The object name is a pure function of the owner and its pinned home region
 * (`NANOCODEX_ACCOUNT_HOMES`, present in every Worker that addresses
 * accounts). Callers never consult another account object to find it.
 * A pinned home pulls its state once from the previous object while blocking
 * all events, then wipes that object; nothing addresses it afterwards.
 */
import type { AccountHostedTools } from "./account-hosted-tools";

export const ACCOUNT_PLACEMENT_KEY = "account_placement_v1";
/** Carried alarm of an adopted export, armed by the adopting instance. */
export const ACCOUNT_ADOPTED_ALARM_KEY = "account_placement_alarm_v1";
/** Home row: source whose wipe is not yet confirmed. */
export const ACCOUNT_WIPE_PENDING_KEY = "account_placement_wipe_pending_v1";
const PLACEMENT_KEYS = new Set([ACCOUNT_PLACEMENT_KEY, ACCOUNT_ADOPTED_ALARM_KEY, ACCOUNT_WIPE_PENDING_KEY]);
export const ACCOUNT_PLACEMENT_REGIONS: ReadonlySet<string> = new Set(["wnam", "enam", "sam", "weur", "eeur", "apac", "oc", "afr", "me"]);
/** "~" and "/" are outside the owner-id alphabet, so names never collide. */
export const HOME_ACCOUNT_PREFIX = "~home/v1/";
const OWNER_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;

export type AccountName =
  | Readonly<{ kind: "owner"; owner: string; name: string }>
  | Readonly<{ kind: "home"; owner: string; region: DurableObjectLocationHint; name: string }>;

export function homeAccountName(region: string, owner: string): string {
  if (!ACCOUNT_PLACEMENT_REGIONS.has(region) || !OWNER_ID.test(owner)) throw new TypeError("invalid account placement");
  return `${HOME_ACCOUNT_PREFIX}${region}/${owner}`;
}

export function parseAccountName(name: unknown): AccountName | undefined {
  if (typeof name !== "string") return undefined;
  if (OWNER_ID.test(name)) return { kind: "owner", owner: name, name };
  if (!name.startsWith(HOME_ACCOUNT_PREFIX)) return undefined;
  const rest = name.slice(HOME_ACCOUNT_PREFIX.length);
  const slash = rest.indexOf("/");
  if (slash < 0) return undefined;
  const region = rest.slice(0, slash), owner = rest.slice(slash + 1);
  if (!ACCOUNT_PLACEMENT_REGIONS.has(region) || !OWNER_ID.test(owner)) return undefined;
  return { kind: "home", owner, region: region as DurableObjectLocationHint, name };
}

export type HomesEnv = { NANOCODEX_ACCOUNT_HOMES?: string };
type Pin = Readonly<{ name: string; source: string }>;

/** `owner:region` or `owner:region<previousRegion` (re-home from a previous home). */
function pin(env: HomesEnv, owner: string): Pin | undefined {
  for (const entry of (env.NANOCODEX_ACCOUNT_HOMES ?? "").split(",")) {
    const [pinned, placement] = entry.trim().split(":");
    if (pinned !== owner || !placement) continue;
    const [region, previous] = placement.split("<");
    if (!region || !ACCOUNT_PLACEMENT_REGIONS.has(region)) continue;
    return { name: homeAccountName(region, owner),
      source: previous && ACCOUNT_PLACEMENT_REGIONS.has(previous) ? homeAccountName(previous, owner) : owner };
  }
  return undefined;
}

/** The owner's account object name: a pure function of deploy-time placement. */
export function accountHomeName(env: HomesEnv, owner: string): string {
  return pin(env, owner)?.name ?? owner;
}

/** The object a pinned home adopts from, once. */
export function accountSourceName(env: HomesEnv, owner: string): string | undefined {
  const placement = pin(env, owner);
  return placement && placement.source !== placement.name ? placement.source : undefined;
}

/** How an owner-namespace object participates, from its own name alone. */
export function accountRole(name: string | undefined, env: HomesEnv): "current" | "home" | "retired" {
  const parsed = parseAccountName(name);
  if (!parsed) return "current";
  const canonical = accountHomeName(env, parsed.owner);
  if (canonical === parsed.name) return parsed.kind === "home" ? "home" : "current";
  return "retired";
}

export type AccountToolsNamespace = Readonly<{ getByName(owner: string): DurableObjectStub<AccountHostedTools> }>;
type RawNamespace = DurableObjectNamespace<AccountHostedTools>;

/** Raw stub for an exact account object name. */
export function accountObjectStub(namespace: RawNamespace, name: string): DurableObjectStub<AccountHostedTools> {
  const parsed = parseAccountName(name);
  return parsed?.kind === "home"
    ? namespace.get(namespace.idFromName(name), { locationHint: parsed.region })
    : namespace.getByName(name);
}

/** Owner-addressed account namespace resolving the canonical object directly. */
export function accountTools(env: HomesEnv & { NANOCODEX_ACCOUNT_TOOLS?: RawNamespace }): AccountToolsNamespace {
  const raw = env.NANOCODEX_ACCOUNT_TOOLS;
  if (!raw) throw new Error("account tools binding is unavailable");
  return { getByName: (owner: string) => accountObjectStub(raw, parseAccountName(owner)?.kind === "owner" ? accountHomeName(env, owner) : owner) };
}

/** Stable export of a retired object, read in pages (the object never mutates). */
export type AccountManifest = Readonly<{
  tables: readonly Readonly<{ name: string; sql: string; columns: readonly string[]; rows: number }>[];
  indexes: readonly string[];
  kv: readonly [string, unknown][];
  alarm: number | null;
}>;
export type AccountRows = readonly unknown[][];
export const ACCOUNT_EXPORT_PAGE_ROWS = 10_000;
export const ACCOUNT_EXPORT_PAGE_BYTES = 8 * 1024 * 1024;

const EXCLUDED_TABLE = /^(sqlite_|_cf_|diagnostic_)/;
const quote = (name: string) => `"${name.replaceAll('"', '""')}"`;
type Bytes = { $bytes: number[] };
const encodeValue = (value: unknown) => value instanceof ArrayBuffer ? { $bytes: [...new Uint8Array(value)] } : value;
const decodeValue = (value: unknown) => value && typeof value === "object" && Array.isArray((value as Bytes).$bytes)
  ? new Uint8Array((value as Bytes).$bytes).buffer : value as SqlStorageValue;

export function exportManifest(storage: DurableObjectStorage, alarm: number | null): AccountManifest {
  const objects = storage.sql.exec<{ type: string; name: string; sql: string | null }>(
    "SELECT type,name,sql FROM sqlite_master WHERE type IN ('table','index') ORDER BY rowid").toArray();
  const tables = objects.filter(entry => entry.type === "table" && entry.sql && !EXCLUDED_TABLE.test(entry.name)).map(entry => {
    const columns = storage.sql.exec(`SELECT * FROM ${quote(entry.name)} LIMIT 0`).columnNames;
    const rows = storage.sql.exec<{ n: number }>(`SELECT COUNT(*) AS n FROM ${quote(entry.name)}`).one().n;
    return { name: entry.name, sql: entry.sql!, columns, rows };
  });
  const indexes = objects.filter(entry => entry.type === "index" && entry.sql && !EXCLUDED_TABLE.test(entry.name)).map(entry => entry.sql!);
  const kv = [...storage.kv.list()].filter(([key]) => !PLACEMENT_KEYS.has(key)) as [string, unknown][];
  return { tables, indexes, kv, alarm };
}

export function exportRows(storage: DurableObjectStorage, table: string, offset: number): AccountRows {
  const known = storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table' AND name=?", table).one().n;
  if (!known || EXCLUDED_TABLE.test(table) || !Number.isSafeInteger(offset) || offset < 0) throw new Error("invalid account export page");
  // Byte-bounded pages keep every RPC well under its message limit and keep
  // the whole pull inside one blockConcurrencyWhile window.
  const page: unknown[][] = [];
  let bytes = 0;
  for (const row of storage.sql.exec(`SELECT * FROM ${quote(table)} LIMIT -1 OFFSET ?`, offset).raw()) {
    const encoded = row.map(encodeValue);
    bytes += JSON.stringify(encoded).length;
    page.push(encoded);
    if (page.length >= ACCOUNT_EXPORT_PAGE_ROWS || bytes >= ACCOUNT_EXPORT_PAGE_BYTES) break;
  }
  return page;
}

/** Replaces this object's owner state. Must run inside transactionSync. */
export function importAccountStorage(storage: DurableObjectStorage, manifest: AccountManifest, rows: ReadonlyMap<string, AccountRows>): void {
  for (const entry of storage.sql.exec<{ name: string }>("SELECT name FROM sqlite_master WHERE type='table'").toArray()) {
    if (!EXCLUDED_TABLE.test(entry.name)) storage.sql.exec(`DROP TABLE IF EXISTS ${quote(entry.name)}`);
  }
  for (const [key] of [...storage.kv.list()]) storage.kv.delete(key);
  for (const table of manifest.tables) {
    if (EXCLUDED_TABLE.test(table.name) || !/^CREATE TABLE/i.test(table.sql)) throw new Error("invalid account export");
    storage.sql.exec(table.sql);
    const values = rows.get(table.name) ?? [];
    if (values.length !== table.rows) throw new Error("incomplete account export");
    if (!values.length) continue;
    const statement = `INSERT INTO ${quote(table.name)} (${table.columns.map(quote).join(",")}) VALUES (${table.columns.map(() => "?").join(",")})`;
    for (const row of values) storage.sql.exec(statement, ...row.map(decodeValue));
  }
  for (const sql of manifest.indexes) if (/^CREATE (UNIQUE )?INDEX/i.test(sql)) {
    storage.sql.exec(sql.replace(/^CREATE (UNIQUE )?INDEX (IF NOT EXISTS )?/i, (_, unique) => `CREATE ${unique ?? ""}INDEX IF NOT EXISTS `));
  }
  for (const [key, value] of manifest.kv) if (!PLACEMENT_KEYS.has(key)) storage.kv.put(key, value);
}
