import {
  ACCOUNT_ADOPTED_ALARM_KEY, ACCOUNT_MIGRATING, ACCOUNT_MIGRATING_HEADER, ACCOUNT_MOVED, ACCOUNT_MOVED_HEADER,
  ACCOUNT_PLACEMENT_KEY, ACCOUNT_REHOME_COOLDOWN_MS, ACCOUNT_REJECTED_KEY, HOME_ACCOUNT_PREFIX,
  accountObjectStub, exportAccountStorage, homeAccountName, importAccountStorage, parseAccountName, validPlacement,
  type AccountExport, type AccountPlacement,
} from "./account-placement";
import type { AccountHostedTools } from "./account-hosted-tools";

const REHOME_IDLE_MS = 2_000;
const RESUME_THROTTLE_MS = 2_000;
const REJECTED_BACKOFF_MS = 60 * 60_000;
const MAX_EXPORT_BYTES = 16 * 1024 * 1024;

export type PlacementHooks = Readonly<{
  ctx: DurableObjectState;
  namespace: () => DurableObjectNamespace<AccountHostedTools> | undefined;
  owner: () => string | undefined;
  /** Single region where the owner's native Hands publish, if unanimous. */
  desiredRegion: () => string | undefined;
  pendingCalls: () => boolean;
  enabled: () => boolean;
}>;

export type PlacementStatus = Readonly<{ adopted: boolean; rejected: boolean; state: AccountPlacement["state"] | "empty" }>;

/** Owner-object placement: gating, idle re-home and adoption. Relays never construct one. */
export class AccountPlacementController {
  #value?: AccountPlacement;
  #inflight = 0;
  #timer?: ReturnType<typeof setTimeout>;
  #running?: Promise<unknown>;
  #resumeAt = 0;
  #blockedUntil = 0;
  /** Adopted and awaiting reset: refuse everything with stale caches. */
  #resetting = false;

  constructor(private readonly hooks: PlacementHooks) {
    const alarm = hooks.ctx.storage.kv.get<number>(ACCOUNT_ADOPTED_ALARM_KEY);
    if (typeof alarm === "number") void hooks.ctx.blockConcurrencyWhile(async () => {
      const current = await hooks.ctx.storage.getAlarm();
      if (current === null || current > alarm) await hooks.ctx.storage.setAlarm(alarm);
      hooks.ctx.storage.kv.delete(ACCOUNT_ADOPTED_ALARM_KEY);
    });
  }

  placement(): AccountPlacement {
    if (this.#value) return this.#value;
    const stored = validPlacement(this.hooks.ctx.storage.kv.get(ACCOUNT_PLACEMENT_KEY));
    const name = (this.hooks.ctx.id as { name?: string }).name;
    const home = name?.startsWith(HOME_ACCOUNT_PREFIX) ? parseAccountName(name) : undefined;
    // An unadopted home names the legacy object; callers only reach it by mistake.
    return this.#value = stored ?? (home ? { state: "moved", target: home.owner, moved_at: 0 } : { state: "active" });
  }

  #set(value: AccountPlacement): void {
    this.hooks.ctx.storage.kv.put(ACCOUNT_PLACEMENT_KEY, value);
    this.#value = value;
  }

  /** Non-active placement, checked before any work. */
  refusal(): Exclude<AccountPlacement, { state: "active" }> | undefined {
    if (this.#resetting) return { state: "migrating", target: "", migration_id: "", started_at: 0, source: "" };
    const placement = this.placement();
    if (placement.state === "active") return undefined;
    if (placement.state === "migrating") this.#resume();
    return placement;
  }

  refusalResponse(): Response | undefined {
    const refusal = this.refusal();
    if (!refusal) return undefined;
    return refusal.state === "moved"
      ? Response.json({ error: ACCOUNT_MOVED }, { status: 421, headers: { [ACCOUNT_MOVED_HEADER]: refusal.target } })
      : Response.json({ error: ACCOUNT_MIGRATING }, { status: 503, headers: { [ACCOUNT_MIGRATING_HEADER]: "1", "retry-after": "1" } });
  }

  refusalError(): Error | undefined {
    const refusal = this.refusal();
    return refusal && new Error(refusal.state === "moved" ? `${ACCOUNT_MOVED}:${refusal.target}` : ACCOUNT_MIGRATING);
  }

  enter(): void { this.#inflight += 1; }
  leave(): void {
    this.#inflight -= 1;
    if (this.#inflight === 0) this.#schedule();
  }

  /** Home name this object should move to, or undefined. */
  target(now = Date.now()): string | undefined {
    const placement = this.placement(), owner = this.hooks.owner();
    if (placement.state !== "active" || !owner || !this.hooks.enabled() || now < this.#blockedUntil) return undefined;
    if (placement.activated_at !== undefined && now - placement.activated_at < ACCOUNT_REHOME_COOLDOWN_MS) return undefined;
    const region = this.hooks.desiredRegion();
    if (!region) return undefined;
    let target: string;
    try { target = homeAccountName(region, owner); } catch { return undefined; }
    return target === this.#name() ? undefined : target;
  }

  #name(): string {
    const placement = this.placement();
    return placement.state === "active" && placement.name ? placement.name : this.hooks.owner()!;
  }

  #quiet(): boolean {
    return this.#inflight === 0 && !this.hooks.pendingCalls() && this.placement().state === "active";
  }

  #schedule(): void {
    if (this.#timer || this.#running || !this.target()) return;
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      const target = this.target();
      if (target && this.#quiet()) this.#track(this.rehome(target).then(() => undefined));
    }, REHOME_IDLE_MS);
  }

  #track(work: Promise<unknown>): void {
    const running = work.catch(error => console.warn({ type: "account.placement.failed",
      error: error instanceof Error ? error.message : String(error) })).finally(() => {
      if (this.#running === running) this.#running = undefined;
    });
    this.#running = running;
  }

  /** Freeze, export and hand this account to `target`. Only runs while idle. */
  async rehome(target: string): Promise<boolean> {
    const owner = this.hooks.owner();
    if (!owner || !this.#quiet()) return false;
    const alarm = await this.hooks.ctx.storage.getAlarm();
    // Re-check after the only await: nothing may run between export and freeze.
    if (!this.#quiet() || parseAccountName(target)?.owner !== owner) return false;
    const source = this.#name(), migration = crypto.randomUUID();
    const data = exportAccountStorage(this.hooks.ctx.storage, { migration_id: migration, source, target, owner }, alarm);
    const bytes = JSON.stringify(data).length;
    if (bytes > MAX_EXPORT_BYTES) {
      this.#blockedUntil = Date.now() + REJECTED_BACKOFF_MS;
      console.warn({ type: "account.placement.skipped", reason: "export_too_large", bytes });
      return false;
    }
    this.#set({ state: "migrating", target, migration_id: migration, started_at: Date.now(), source });
    // Hosts and viewers reconnect through the Worker, which follows the move.
    for (const socket of this.hooks.ctx.getWebSockets()) { try { socket.close(1012, "Account moved"); } catch { /* already closed */ } }
    console.info({ type: "account.placement.migrating", source, target, migration_id: migration, bytes, tables: data.tables.length });
    await this.#finish(data);
    return this.placement().state === "moved";
  }

  #resume(): void {
    if (this.#running || Date.now() < this.#resumeAt) return;
    this.#resumeAt = Date.now() + RESUME_THROTTLE_MS;
    this.#track(this.#finish());
  }

  /** Idempotent adoption and verification; any uncertainty stays frozen. */
  async #finish(prepared?: AccountExport): Promise<void> {
    const placement = this.placement(), namespace = this.hooks.namespace(), owner = this.hooks.owner();
    if (placement.state !== "migrating" || !namespace || !owner) return;
    const data = prepared ?? exportAccountStorage(this.hooks.ctx.storage, { migration_id: placement.migration_id,
      source: placement.source, target: placement.target, owner }, await this.hooks.ctx.storage.getAlarm());
    const home = accountObjectStub(namespace, placement.target);
    // Adoption resets the home after commit, so its own result is never trusted.
    try { await home.adoptAccount(data); } catch { /* verified below */ }
    // A reset object breaks its stubs; verify through a fresh one.
    let status: PlacementStatus;
    try { status = await accountObjectStub(namespace, placement.target).accountPlacement(placement.migration_id); }
    catch (error) {
      // Unknown outcome: stay frozen; the next request resumes the same migration.
      console.warn({ type: "account.placement.unverified", target: placement.target, migration_id: placement.migration_id,
        error: error instanceof Error ? error.message : String(error) });
      return;
    }
    if (this.placement() !== placement) return;
    if (status.adopted) {
      this.#set({ state: "moved", target: placement.target, moved_at: Date.now() });
      console.info({ type: "account.placement.moved", source: placement.source, target: placement.target, migration_id: placement.migration_id });
    } else if (status.rejected) {
      // The home durably refuses this id; resuming here cannot split the account.
      this.#set({ state: "active", name: placement.source });
      this.#blockedUntil = Date.now() + REJECTED_BACKOFF_MS;
      console.warn({ type: "account.placement.rejected", source: placement.source, target: placement.target, migration_id: placement.migration_id });
    }
  }

  #rejected(): string[] {
    const value = this.hooks.ctx.storage.kv.get<unknown>(ACCOUNT_REJECTED_KEY);
    return Array.isArray(value) ? value.filter((id): id is string => typeof id === "string") : [];
  }

  #reject(migration: string): never {
    this.hooks.ctx.storage.kv.put(ACCOUNT_REJECTED_KEY, [...this.#rejected().filter(id => id !== migration), migration].slice(-32));
    throw new Error("account_adopt_rejected");
  }

  status(migration: string): PlacementStatus {
    const stored = validPlacement(this.hooks.ctx.storage.kv.get(ACCOUNT_PLACEMENT_KEY));
    return { adopted: stored?.state === "active" && stored.migration_id === migration,
      rejected: this.#rejected().includes(migration), state: stored?.state ?? "empty" };
  }

  /**
   * Replace this object's state with an export, then reset so every cache
   * reloads from storage. Only an unclaimed object or a moved tombstone adopts.
   */
  async adopt(data: AccountExport): Promise<void> {
    const migration = typeof data?.migration_id === "string" ? data.migration_id : undefined;
    if (!migration) throw new Error("account_adopt_rejected");
    const stored = validPlacement(this.hooks.ctx.storage.kv.get(ACCOUNT_PLACEMENT_KEY));
    if (stored?.state === "active" && stored.migration_id === migration) return;
    if (this.#rejected().includes(migration)) throw new Error("account_adopt_rejected");
    const target = parseAccountName(data.target), source = parseAccountName(data.source);
    const claimed = this.hooks.ctx.storage.kv.get<string>("owner_id");
    const name = (this.hooks.ctx.id as { name?: string }).name;
    if (!target || !source || target.owner !== data.owner || source.owner !== data.owner || target.name === source.name
      || (name !== undefined && name !== target.name) || !Array.isArray(data.tables) || !Array.isArray(data.kv)
      || (stored ? stored.state !== "moved" : claimed !== undefined)
      || (claimed !== undefined && claimed !== data.owner) || this.#inflight > 0) this.#reject(migration);
    try {
      this.hooks.ctx.storage.transactionSync(() => {
        importAccountStorage(this.hooks.ctx.storage, data);
        if (typeof data.alarm === "number") this.hooks.ctx.storage.kv.put(ACCOUNT_ADOPTED_ALARM_KEY, data.alarm);
        this.hooks.ctx.storage.kv.put(ACCOUNT_PLACEMENT_KEY, { state: "active", name: target.name, activated_at: Date.now(),
          migration_id: migration, source: source.name } satisfies AccountPlacement);
      });
    } catch (error) {
      console.warn({ type: "account.placement.import_failed", error: error instanceof Error ? error.message : String(error) });
      this.#reject(migration);
    }
    console.info({ type: "account.placement.adopted", source: source.name, target: target.name, migration_id: migration });
    // Constructor caches (owner, broker, directory, shares) must reload from the
    // import. Commit first (a reset discards unconfirmed writes); refuse meanwhile.
    this.#resetting = true;
    try { await this.hooks.ctx.storage.sync(); } finally { this.hooks.ctx.abort("account adopted"); }
  }
}
