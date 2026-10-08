import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";
import type { CredentialVaultEnv } from "./credential-vault";
import type { UserCredentialBroker } from "./broker";
import { RoutedUserBroker } from "./broker-router";

/** Regions a trusted caller may place a replica in. */
export const SNAPSHOT_REGIONS: ReadonlySet<string> = new Set(["wnam", "enam", "sam", "weur", "eeur", "apac", "oc"]);
const OWNER = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
/** Retired credential-cache rows; deleted on invalidation. */
const SNAPSHOT_KEY = "snapshot-v1";
const CLAUDE_SNAPSHOT_KEY = "claude-snapshot-v1";
const FLOOR_KEY = "floor-v1";

export interface CredentialSnapshotEnv extends CredentialVaultEnv {
  USER_CREDENTIALS: DurableObjectNamespace<UserCredentialBroker>;
  USER_CREDENTIAL_SNAPSHOTS?: DurableObjectNamespace<UserCredentialSnapshot>;
}

export type PrewarmOutcome = "warm" | "filled" | "unavailable" | "invalid" | "unsupported";

/** Pending auth-only model upgrades held by this regional holder. */
const MAX_PENDING_UPGRADES = 8;
const PREPARED_UPGRADE_TTL_MS = 10_000;
export const PREPARED_UPGRADE_HEADER = "x-nanocodex-prepared-model-upgrade";
export const PREPARED_UPGRADE_URL = "https://snapshot.internal/v1/prepared-model-upgrade";
const PREPARED_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const FINGERPRINT = /^[0-9a-f]{64}$/;
export type PreparedUpgradeAuthority = Readonly<{ subject: string; owner: string; region: DurableObjectLocationHint }>;
/** Injected by egress.ts at module load (avoids a circular import). Runs the
 * exact SessionModelEgress handshake using this holder's local credential. */
export type PreparedUpgradeStarter = (request: Request, env: CredentialSnapshotEnv,
  ctx: Pick<ExecutionContext, "waitUntil">, authority: PreparedUpgradeAuthority) => Promise<Response>;
let preparedUpgradeStarter: PreparedUpgradeStarter | undefined;
export function registerPreparedUpgradeStarter(starter: PreparedUpgradeStarter): void { preparedUpgradeStarter = starter; }
export type PrepareUpgradeResult = Readonly<{ status: "prepared"; id: string } | { status: "invalid" | "busy" | "unsupported" }>;
type PendingUpgrade = {
  id: string; subject: string; fingerprint: string; started: number;
  abort: AbortController; timer: ReturnType<typeof setTimeout>;
  result: Promise<Response | undefined>; response?: Response | undefined;
  claimed: boolean; disposed: boolean; transferred: boolean; settled: boolean; released: boolean;
  /** Settles on disposal so a claimed consume never outlives cancel/TTL/invalidation. */
  ended: Promise<undefined>; end: () => void;
};

/** SHA-256 over the exact sorted (lowercased) header list. */
export async function headerFingerprint(headers: Iterable<[string, string]>): Promise<string> {
  const entries = [...headers].map(([name, value]) => [name.toLowerCase(), value] as const)
    .sort(([a, x], [b, y]) => a < b ? -1 : a > b ? 1 : x < y ? -1 : x > y ? 1 : 0);
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify(entries)));
  return [...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, "0")).join("");
}

export function snapshotName(region: string, owner: string): string {
  return `${region}:${owner}`;
}

export function snapshotStub(
  env: Pick<CredentialSnapshotEnv, "USER_CREDENTIAL_SNAPSHOTS">,
  owner: string,
  region: string,
): DurableObjectStub<UserCredentialSnapshot> | undefined {
  if (!env.USER_CREDENTIAL_SNAPSHOTS || !OWNER.test(owner) || !SNAPSHOT_REGIONS.has(region)) return undefined;
  return env.USER_CREDENTIAL_SNAPSHOTS.getByName(snapshotName(region, owner),
    { locationHint: region as DurableObjectLocationHint });
}

/**
 * Regional holder for pending auth-only model upgrades, next to the Session.
 * It no longer caches credentials: the canonical UserCredentialBroker is
 * itself placed next to the user, so every credential read goes there. The
 * invalidation fence remains so the canonical lease registry can drain
 * holders that cached credentials before this change.
 */
export class UserCredentialSnapshot extends DurableObject<CredentialSnapshotEnv> {
  #loaded: Promise<void> | undefined;
  #floor = 0;
  readonly #pending = new Map<string, PendingUpgrade>();
  #activeUpgrades = 0;
  /** Preparations are refused while any invalidation is in flight. */
  #invalidating = 0;

  constructor(state: DurableObjectState, env: CredentialSnapshotEnv) {
    super(state, env);
  }

  /**
   * Called only by the canonical broker. Durably raises the floor and drops
   * the snapshot before acknowledging; it never calls back into the broker,
   * so the broker may await it while holding its own queue.
   */
  async invalidate(owner: string, region: string, epoch: number): Promise<boolean> {
    if (!this.#boundTo(owner, region) || !Number.isSafeInteger(epoch) || epoch < 0) return false;
    // Unconsumed handshakes were authorized by the superseded credential. New
    // preparations are refused until the floor is durable; cancel again after
    // every await so nothing started in between escapes.
    this.#invalidating += 1;
    try {
      this.#disposeAllUpgrades("invalidated");
      await this.#load();
      if (epoch > this.#floor) this.#floor = epoch;
      this.#disposeAllUpgrades("invalidated");
      // One implicit transaction; the output gate holds the ACK until durable.
      await this.ctx.storage.put(FLOOR_KEY, this.#floor);
      await this.ctx.storage.delete([SNAPSHOT_KEY, CLAUDE_SNAPSHOT_KEY]);
      this.#disposeAllUpgrades("invalidated");
      return true;
    } finally { this.#invalidating -= 1; }
  }

  /**
   * Private Session preparation: starts ONE auth-only handshake and returns an
   * opaque one-shot id immediately (the ACK). Never sends a frame. The caller
   * (SessionModelEgress) has already validated Session authority and computed
   * the exact header fingerprint; owner/region must name this object.
   */
  async prepareModelUpgrade(owner: string, region: string, subject: string, fingerprint: string,
    headers: [string, string][]): Promise<PrepareUpgradeResult> {
    if (!this.#boundTo(owner, region) || typeof subject !== "string" || typeof fingerprint !== "string"
      || !FINGERPRINT.test(fingerprint) || !Array.isArray(headers)) return { status: "invalid" };
    const starter = preparedUpgradeStarter;
    if (!starter) return { status: "unsupported" };
    if (this.#invalidating > 0 || this.#activeUpgrades >= MAX_PENDING_UPGRADES) return { status: "busy" };
    // Register with the (local) canonical broker first, so any credential
    // change after this point closes the handshake before it is acknowledged.
    let registration: { status: number; epoch: number };
    try {
      registration = await new RoutedUserBroker(this.env, owner, { claim: region }).registerUpgradeHolder(owner, region);
    } catch { return { status: "unsupported" }; }
    if (registration.status !== 200 || !Number.isSafeInteger(registration.epoch)) return { status: "unsupported" };
    await this.#load();
    if (registration.epoch < this.#floor || this.#invalidating > 0
      || this.#activeUpgrades >= MAX_PENDING_UPGRADES) return { status: "busy" };
    const id = crypto.randomUUID();
    const abort = new AbortController();
    const pending: PendingUpgrade = { id, subject, fingerprint, started: Date.now(), abort,
      timer: setTimeout(() => this.#disposeUpgrade(pending, "expired"), PREPARED_UPGRADE_TTL_MS),
      result: Promise.resolve(undefined), claimed: false, disposed: false, transferred: false, settled: false, released: false,
      ended: Promise.resolve(undefined), end: () => {} };
    pending.ended = new Promise(resolve => { pending.end = () => resolve(undefined); });
    this.#activeUpgrades += 1;
    let started: Promise<Response>;
    try {
      const request = new Request("https://nanocodex.internal/v1/responses", { headers, signal: abort.signal });
      started = starter(request, this.env, this.ctx,
        { subject, owner, region: region as DurableObjectLocationHint });
    } catch (error) { started = Promise.reject(error); }
    pending.result = started.then(response => {
      pending.response = response;
      if (pending.disposed || response.status !== 101 || !response.webSocket) {
        this.#disposeUpgrade(pending, pending.disposed ? "late" : "unavailable");
        return undefined;
      }
      this.#observeUpgrade(pending, "connected");
      return response;
    }, () => { this.#disposeUpgrade(pending, "failed"); return undefined; })
      .finally(() => { pending.settled = true; this.#releaseUpgradeCapacity(pending); });
    this.#pending.set(id, pending);
    // Keep the handshake's lifetime explicit after the ACK returns.
    this.ctx.waitUntil(pending.result.then(() => {}));
    this.#observeUpgrade(pending, "started");
    return { status: "prepared", id };
  }

  /** Best-effort early release by the owning Session. */
  async cancelModelUpgrade(owner: string, region: string, id: string, subject: string,
    fingerprint: string): Promise<boolean> {
    if (!this.#boundTo(owner, region) || typeof id !== "string") return false;
    const pending = this.#pending.get(id);
    if (!pending || pending.subject !== subject || pending.fingerprint !== fingerprint) return false;
    // A claimed but untransferred handshake is still cancellable: the Session
    // may retire while consumption awaits it; fetch() rechecks disposal.
    if (pending.transferred) return false;
    this.#disposeUpgrade(pending, "cancelled");
    return true;
  }

  /** Consumption must use fetch: a 101 WebSocket is not RPC-serializable. */
  override async fetch(request: Request): Promise<Response> {
    const headers = request.headers;
    const id = headers.get(PREPARED_UPGRADE_HEADER) ?? "";
    const owner = headers.get("x-nanocodex-session-model-owner");
    const region = headers.get("x-nanocodex-model-region");
    const subject = headers.get("x-nanocodex-subject");
    const fingerprint = headers.get("x-nanocodex-upgrade-fingerprint") ?? "";
    if (request.method !== "GET" || request.url !== PREPARED_UPGRADE_URL || !PREPARED_ID.test(id)
      || headers.get("upgrade")?.toLowerCase() !== "websocket"
      || !FINGERPRINT.test(fingerprint) || !this.#boundTo(owner, region)) return new Response(null, { status: 403 });
    const pending = this.#pending.get(id);
    if (!pending || pending.claimed || pending.disposed) return new Response(null, { status: 404 });
    // Hard expiry; the timer alone may lag.
    if (this.#expired(pending)) { this.#disposeUpgrade(pending, "expired"); return new Response(null, { status: 404 }); }
    // One-shot: any claim, matching or not, ends this preparation.
    pending.claimed = true;
    if (pending.subject !== subject || pending.fingerprint !== fingerprint) {
      this.#disposeUpgrade(pending, "mismatch");
      return new Response(null, { status: 409 });
    }
    // A stalled credential grant or handshake must not hold the claim past
    // disposal; a late response is still closed by the result continuation.
    const response = await Promise.race([pending.result, pending.ended]);
    if (!response || pending.disposed || this.#expired(pending) || this.#invalidating > 0) {
      this.#disposeUpgrade(pending, this.#expired(pending) ? "expired" : "stale");
      return new Response(null, { status: 404 });
    }
    pending.transferred = true;
    this.#releaseUpgradeCapacity(pending);
    clearTimeout(pending.timer);
    this.#pending.delete(id);
    this.#observeUpgrade(pending, "consumed");
    return response;
  }

  #expired(pending: PendingUpgrade): boolean {
    return Date.now() >= pending.started + PREPARED_UPGRADE_TTL_MS;
  }

  #disposeAllUpgrades(outcome: string): void {
    for (const pending of [...this.#pending.values()]) this.#disposeUpgrade(pending, outcome);
  }

  // Cancellation can settle the consumer before an unabortable credential
  // RPC returns. Keep that outstanding work charged to the capacity limit.
  #releaseUpgradeCapacity(pending: PendingUpgrade): void {
    if (!pending.released && pending.settled && (pending.disposed || pending.transferred)) {
      pending.released = true;
      this.#activeUpgrades -= 1;
    }
  }

  #disposeUpgrade(pending: PendingUpgrade, outcome: string): void {
    if (pending.transferred) return;
    if (!pending.disposed) {
      pending.disposed = true;
      this.#releaseUpgradeCapacity(pending);
      pending.end();
      clearTimeout(pending.timer);
      if (this.#pending.get(pending.id) === pending) this.#pending.delete(pending.id);
      pending.abort.abort();
      this.#observeUpgrade(pending, outcome);
    }
    // Also closes a handshake that completed after disposal. No frames are sent.
    const response = pending.response;
    pending.response = undefined;
    if (response?.webSocket) {
      try { response.webSocket.accept(); } catch { /* already accepted/closed */ }
      try { response.webSocket.close(1000, "Preparation ended"); } catch { /* disconnected */ }
    } else { void response?.body?.cancel().catch(() => {}); }
  }

  #observeUpgrade(pending: PendingUpgrade, outcome: string): void {
    // Subject correlates with the owning Session's log; never id or headers.
    console.info({ type: "egress.prepared_model_upgrade", outcome, subject: pending.subject, pending: this.#pending.size,
      at_ms: Date.now(), elapsed_ms: Date.now() - pending.started });
  }

  #boundTo(owner: unknown, region: unknown): boolean {
    if (typeof owner !== "string" || typeof region !== "string" || !OWNER.test(owner)
      || !SNAPSHOT_REGIONS.has(region) || !this.env.USER_CREDENTIAL_SNAPSHOTS) return false;
    return this.env.USER_CREDENTIAL_SNAPSHOTS.idFromName(snapshotName(region, owner)).equals(this.ctx.id);
  }

  #load(): Promise<void> {
    return this.#loaded ??= (async () => {
      const floor = await this.ctx.storage.get<unknown>(FLOOR_KEY);
      if (typeof floor === "number" && Number.isSafeInteger(floor) && floor > this.#floor) this.#floor = floor;
    })().catch((error) => { this.#loaded = undefined; throw error; });
  }
}

/**
 * Bound only to the managed Worker. It overlaps the canonical broker's cold
 * activation (and first adoption) with Session activation; outcome only.
 */
export class SessionCredentialPrewarm extends WorkerEntrypoint<CredentialSnapshotEnv> {
  async prewarm(input: unknown): Promise<{ outcome: PrewarmOutcome }> {
    const owner = input && typeof input === "object" ? (input as { owner?: unknown }).owner : undefined;
    const region = input && typeof input === "object" ? (input as { region?: unknown }).region : undefined;
    if (typeof owner !== "string" || typeof region !== "string") return { outcome: "invalid" };
    if (!OWNER.test(owner) || !SNAPSHOT_REGIONS.has(region)) return { outcome: "invalid" };
    try {
      // Activates, and on first use adopts, the user's canonical broker in
      // this region, overlapping the cold start with Session activation.
      return { outcome: await new RoutedUserBroker(this.env, owner, { claim: region }).warm() };
    } catch {
      return { outcome: "unavailable" };
    }
  }
}
