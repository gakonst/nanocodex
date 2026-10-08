import {
  ACCOUNT_ADOPTED_ALARM_KEY, ACCOUNT_PLACEMENT_KEY, ACCOUNT_WIPE_PENDING_KEY, ACCOUNT_EXPORT_PAGE_ROWS,
  accountHomeName, accountObjectStub, accountSourceName, exportManifest, exportRows, importAccountStorage, parseAccountName,
  type AccountManifest, type AccountRows, type HomesEnv,
} from "./account-placement";
import type { AccountHostedTools } from "./account-hosted-tools";

type PlacementEnv = HomesEnv & { NANOCODEX_ACCOUNT_TOOLS?: DurableObjectNamespace<AccountHostedTools> };
type Active = Readonly<{ state: "active"; name: string; source: string; adopted_at: number }>;

function active(ctx: DurableObjectState): Active | undefined {
  const row = ctx.storage.kv.get<Active>(ACCOUNT_PLACEMENT_KEY);
  return row?.state === "active" && typeof row.name === "string" ? row : undefined;
}

/** Whether this pinned home still has to pull its account. */
export function homeNeedsAdoption(ctx: DurableObjectState): boolean {
  return active(ctx)?.name !== ctx.id.name;
}

/**
 * Pull the account from its single source while the caller blocks every
 * event: manifest, all pages, one import transaction, commit, then wipe the
 * source. Any failure throws and leaves this home empty (fail closed); the
 * retired source never changes, so the next activation repeats identically.
 */
export async function adoptAccount(ctx: DurableObjectState, env: PlacementEnv): Promise<void> {
  const name = ctx.id.name, parsed = parseAccountName(name), namespace = env.NANOCODEX_ACCOUNT_TOOLS;
  if (!name || parsed?.kind !== "home" || accountHomeName(env, parsed.owner) !== name || !namespace) throw new Error("account home unavailable");
  const source = accountSourceName(env, parsed.owner);
  if (!source) throw new Error("account home unavailable");
  const started = Date.now();
  const stub = accountObjectStub(namespace, source) as unknown as {
    releaseManifest(owner: string, target: string): Promise<AccountManifest>;
    releaseRows(owner: string, target: string, table: string, offset: number): Promise<AccountRows>;
  };
  const manifest = await stub.releaseManifest(parsed.owner, name);
  const rows = new Map<string, AccountRows>();
  for (const table of manifest.tables) {
    const values: unknown[][] = [];
    while (values.length < table.rows) {
      const page = await stub.releaseRows(parsed.owner, name, table.name, values.length);
      if (!page.length) throw new Error("incomplete account export");
      values.push(...page.map(row => [...row]));
    }
    rows.set(table.name, values);
  }
  ctx.storage.transactionSync(() => {
    importAccountStorage(ctx.storage, manifest, rows);
    if (typeof manifest.alarm === "number") ctx.storage.kv.put(ACCOUNT_ADOPTED_ALARM_KEY, manifest.alarm);
    ctx.storage.kv.put(ACCOUNT_WIPE_PENDING_KEY, source);
    ctx.storage.kv.put(ACCOUNT_PLACEMENT_KEY, { state: "active", name, source, adopted_at: Date.now() } satisfies Active);
  });
  // The source is wiped only after this import is durable.
  await ctx.storage.sync();
  if (typeof manifest.alarm === "number") {
    const current = await ctx.storage.getAlarm();
    if (current === null || current > manifest.alarm) await ctx.storage.setAlarm(manifest.alarm);
  }
  ctx.storage.kv.delete(ACCOUNT_ADOPTED_ALARM_KEY);
  console.info({ type: "account.placement.adopted", source, target: name, tables: manifest.tables.length,
    rows: manifest.tables.reduce((sum, table) => sum + table.rows, 0), duration_ms: Date.now() - started });
  await wipeSource(ctx, env);
}

/** Wipe the adopted source if not yet confirmed. Failures retry on the next activation. */
export async function wipeSource(ctx: DurableObjectState, env: PlacementEnv): Promise<void> {
  const source = ctx.storage.kv.get<string>(ACCOUNT_WIPE_PENDING_KEY), parsed = parseAccountName(ctx.id.name);
  if (!source || !parsed || !env.NANOCODEX_ACCOUNT_TOOLS) return;
  try {
    await accountObjectStub(env.NANOCODEX_ACCOUNT_TOOLS, source).wipeRetiredAccount(parsed.owner, ctx.id.name!);
    ctx.storage.kv.delete(ACCOUNT_WIPE_PENDING_KEY);
    console.info({ type: "account.placement.source_wiped", source, target: ctx.id.name });
  } catch (error) {
    console.warn({ type: "account.placement.wipe_deferred", source, error: error instanceof Error ? error.message : String(error) });
  }
}

/** Retired side: only the canonical home of the same owner may read or wipe it. */
function authorizeRetired(ctx: DurableObjectState, env: HomesEnv, owner: string, target: string): void {
  const self = parseAccountName(ctx.id.name);
  if (!self || self.owner !== owner || accountHomeName(env, owner) !== target || target === ctx.id.name
    || accountSourceName(env, owner) !== ctx.id.name) throw new Error("account unavailable");
}

export async function retiredManifest(ctx: DurableObjectState, env: HomesEnv, owner: string, target: string): Promise<AccountManifest> {
  authorizeRetired(ctx, env, owner, target);
  return exportManifest(ctx.storage, await ctx.storage.getAlarm());
}

export function retiredRows(ctx: DurableObjectState, env: HomesEnv, owner: string, target: string, table: string, offset: number): AccountRows {
  authorizeRetired(ctx, env, owner, target);
  return exportRows(ctx.storage, table, offset);
}

export async function wipeRetired(ctx: DurableObjectState, env: HomesEnv, owner: string, target: string): Promise<void> {
  authorizeRetired(ctx, env, owner, target);
  await ctx.storage.deleteAlarm();
  await ctx.storage.deleteAll();
  for (const socket of ctx.getWebSockets()) { try { socket.close(1012, "Account unavailable"); } catch { /* closed */ } }
  console.info({ type: "account.placement.wiped", source: ctx.id.name, target });
}

export { ACCOUNT_EXPORT_PAGE_ROWS };
