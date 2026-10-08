import { accountTools } from "./account-placement";
import { HAND_OWNER_HEADER, HAND_RELAY_REGION_HEADER, handRelayName, isHandRelayRegion,
  type HandRelayLocation, type HandRelayRegion, type RegionalHandEnv } from "./regional-hand-routing";

/** Regional screen signaling: human WebRTC host/viewer sockets live in the
 * ingress region's relay. Owner authority remains on the account DO. */
export type RegionalScreenEnv = RegionalHandEnv & { NANOCODEX_REGIONAL_SCREEN_RELAYS?: string };

export function regionalScreensEnabled(env: RegionalScreenEnv): boolean {
  return env.NANOCODEX_REGIONAL_SCREEN_RELAYS === "true" && !!env.NANOCODEX_HAND_RELAYS && !!env.NANOCODEX_ACCOUNT_TOOLS;
}

/** Connection IDs and generations minted by a regional broker. Clients treat them as opaque IDs. */
export function regionalScreenPrefix(region: HandRelayRegion): string { return `rs.${region}.`; }

export function regionalScreenRegion(id: unknown): HandRelayRegion | undefined {
  if (typeof id !== "string") return undefined;
  const match = /^rs\.([a-z]+)\./.exec(id);
  return match && isHandRelayRegion(match[1]) ? match[1] : undefined;
}

export function regionalScreenRelay(env: RegionalScreenEnv, owner: string, region: HandRelayRegion) {
  return env.NANOCODEX_HAND_RELAYS!.getByName(handRelayName(owner, region), { locationHint: region });
}

/** Internal request to a regional relay. Owner and region come from trusted Worker routing only. */
export function regionalScreenRequest(request: Request, owner: string, region: HandRelayRegion, path: string): Request {
  const headers = new Headers(request.headers);
  headers.set(HAND_OWNER_HEADER, owner);
  headers.set(HAND_RELAY_REGION_HEADER, region);
  const url = new URL(request.url);
  return new Request(`https://account-tools.internal${path}${url.search}`, new Request(request, { headers }));
}

const SCREEN_DIRECTORY_HEADER = "x-nanocodex-screen-directory";
type Brokered = (path: string, init?: { body?: string; directory?: boolean }) => Request;
type ListedSurface = { machine_id: string; generation: string };

/** Region selected by a broker-minted connection ID or generation, if any. */
export function regionalScreenTarget(url: URL, body?: unknown): HandRelayRegion | undefined {
  if (url.pathname.endsWith("/hands/view")) return regionalScreenRegion(url.searchParams.get("generation"));
  if (url.pathname.endsWith("/hands/renew")) return regionalScreenRegion((body as { connection_id?: unknown } | undefined)?.connection_id);
  return undefined;
}

/**
 * Worker routing for already-authenticated account screen requests. `brokered`
 * builds the owner-asserted internal request (handBrokerRequest). Returns
 * undefined when the owner DO should serve the request unchanged.
 * - host: regional relay of the trusted ingress region (flag on), else owner.
 * - view/renew: the region named by the broker-minted ID prefix, else owner.
 * - screens: owner authority + local listing merged with every relay named by
 *   that authority. The flag only selects placement of NEW hosts; a rollback
 *   keeps discovering, viewing and renewing retained regional hosts.
 */
export async function routeRegionalScreens(request: Request, env: RegionalScreenEnv, owner: string, ingress: HandRelayRegion | undefined,
  brokered: Brokered): Promise<Response | undefined> {
  const url = new URL(request.url);
  const path = url.pathname.slice("/v1/account".length);
  if (!env.NANOCODEX_HAND_RELAYS || !env.NANOCODEX_ACCOUNT_TOOLS) return undefined;
  const relay = (region: HandRelayRegion, init?: { body?: string }) =>
    regionalScreenRelay(env, owner, region).fetch(regionalScreenRequest(brokered(path, init), owner, region, path));
  if (path === "/hands/host") {
    return regionalScreensEnabled(env) && ingress ? relay(ingress) : undefined;
  }
  if (path === "/hands/view") {
    const region = regionalScreenTarget(url);
    return region ? relay(region) : undefined;
  }
  if (path === "/hands/renew" && request.method === "POST" && !url.search) {
    // Renewals carry at most 256 bytes; reparse once to select the broker.
    const text = await request.text();
    if (new TextEncoder().encode(text).length > 256) return Response.json({ error: "invalid_request" }, { status: 400 });
    let body: unknown;
    try { body = JSON.parse(text); } catch { body = undefined; }
    const region = regionalScreenTarget(url, body);
    if (region) return relay(region, { body: text });
    return accountTools(env).getByName(owner).fetch(brokered(path, { body: text }));
  }
  if (path === "/hands/screens" && request.method === "GET" && !url.search) {
    const listing = async (response: Response) => response.ok ? (await response.json<{ surfaces: ListedSurface[] }>()).surfaces : undefined;
    // Speculate on the likely relay only when new hosts are placed regionally.
    const speculative = ingress && regionalScreensEnabled(env) ? relay(ingress).then(listing).catch(() => undefined) : undefined;
    const ownerResponse = await accountTools(env).getByName(owner).fetch(brokered(path, { directory: true }));
    if (!ownerResponse.ok) return ownerResponse;
    const value = await ownerResponse.json<{ surfaces: ListedSurface[]; regional_hosts?: Record<string, { region: string; generation: string }> }>();
    const hosts = value.regional_hosts ?? {};
    const regions = [...new Set(Object.values(hosts).map(host => host.region).filter(isHandRelayRegion))];
    const listed = await Promise.all(regions.map(async region => ({ region,
      surfaces: region === ingress && speculative ? await speculative : await relay(region).then(listing).catch(() => undefined) })));
    const surfaces = [...value.surfaces];
    for (const { region, surfaces: regional } of listed) for (const surface of regional ?? []) {
      const host = hosts[surface.machine_id];
      if (host?.region === region && host.generation === surface.generation) surfaces.push(surface);
    }
    const headers = new Headers(ownerResponse.headers);
    headers.delete("content-length");
    return Response.json({ surfaces }, { status: 200, headers });
  }
  return undefined;
}
export { SCREEN_DIRECTORY_HEADER };

type Fence = Readonly<{ location: HandRelayLocation; keep?: string; reason?: ScreenFenceReason }>;
type Row = { machine_id: string; region: string; generation: string; pending_fences: string; watermarks: string };
export type ScreenFenceReason = "host_replaced" | "publisher_revoked";
/** Fence one location's hosts of a machine (except `keep`) and return that
 * location's host-sequence high-water mark, or false when unconfirmed. Legacy
 * fences run synchronously in the owner; regional fences never target a relay
 * that is awaiting the current claim. */
export type ScreenFencer = (location: HandRelayLocation, machineId: string, keep: string | undefined, reason: ScreenFenceReason) => Promise<number | false>;
export type ScreenAuthority = Readonly<{ region: HandRelayLocation; generation: string }>;

/** Owner-DO authority for which (location, generation) may publish a machine's
 * screens. Every broker numbers its host sockets monotonically; a confirmed
 * fence records that location's high-water mark, so any later claim from a
 * socket accepted before the fence is rejected forever (no bounded tombstones).
 * Claims fail closed: nothing is listed while another location's fence is
 * unconfirmed, and the claim is rejected. State is durable before every RPC. */
export class RegionalScreenAuthority {
  #queue: Promise<unknown> = Promise.resolve();
  constructor(private readonly storage: DurableObjectStorage, private readonly fence: ScreenFencer) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_screen_hosts (
      machine_id TEXT PRIMARY KEY, region TEXT NOT NULL, generation TEXT NOT NULL,
      pending_fences TEXT NOT NULL DEFAULT '[]', watermarks TEXT NOT NULL DEFAULT '{}'
    )`);
  }

  #serial<T>(work: () => Promise<T>): Promise<T> {
    const next = this.#queue.then(work, work);
    this.#queue = next.catch(() => undefined);
    return next;
  }
  #row(machineId: string): Row | undefined {
    return this.storage.sql.exec<Row>("SELECT * FROM regional_screen_hosts WHERE machine_id=?", machineId).toArray()[0];
  }
  #save(machineId: string, region: string, generation: string, pending: readonly Fence[], marks: Record<string, number>): void {
    this.storage.sql.exec(`INSERT INTO regional_screen_hosts(machine_id,region,generation,pending_fences,watermarks) VALUES(?,?,?,?,?)
      ON CONFLICT(machine_id) DO UPDATE SET region=excluded.region,generation=excluded.generation,
      pending_fences=excluded.pending_fences,watermarks=excluded.watermarks`,
    machineId, region, generation, JSON.stringify(pending), JSON.stringify(marks));
  }
  async #drain(machineId: string, fences: readonly Fence[], reason: ScreenFenceReason, marks: Record<string, number>): Promise<Fence[]> {
    const remaining: Fence[] = [];
    for (const fence of fences) {
      let mark: number | false = false;
      try { mark = await this.fence(fence.location, machineId, fence.keep, fence.reason ?? reason); } catch { mark = false; }
      if (mark === false || !Number.isSafeInteger(mark)) remaining.push(fence);
      else marks[fence.location] = Math.max(marks[fence.location] ?? 0, mark);
    }
    return remaining;
  }
  static #unique(fences: readonly Fence[]): Fence[] {
    return [...new Map(fences.map(fence => [fence.location, fence])).values()];
  }

  /** Grant `generation` (host socket `sequence`) at `location`, or reject it. */
  claim(machineId: string, location: HandRelayLocation, generation: string, sequence: number): Promise<boolean> {
    return this.#serial(async () => {
      const row = this.#row(machineId);
      const marks: Record<string, number> = row ? JSON.parse(row.watermarks) : {};
      if (!Number.isSafeInteger(sequence) || sequence <= (marks[location] ?? 0)) return false;
      const pending: Fence[] = row ? JSON.parse(row.pending_fences) : [];
      if (row && row.region === location && row.generation === generation && !pending.some(fence => fence.location !== location)) return true;
      // The claimant's own location is replaced by its broker after the grant.
      const own = pending.filter(fence => fence.location === location);
      const cross = RegionalScreenAuthority.#unique([
        ...pending.filter(fence => fence.location !== location).map(fence => ({ location: fence.location, reason: fence.reason })),
        ...(row?.generation && row.region !== location ? [{ location: row.region as HandRelayLocation, reason: "host_replaced" as const }] : []),
        // Legacy hosts can predate authority rows; a regional claim always fences them.
        ...(location !== "legacy" ? [{ location: "legacy" as const, reason: "host_replaced" as const }] : []),
      ]);
      // Fail closed and durable before any RPC: nothing is listed until fences are confirmed.
      this.#save(machineId, row?.region ?? location, "", [...cross, ...own], marks);
      const remaining = await this.#drain(machineId, cross, "host_replaced", marks);
      if (remaining.length) { this.#save(machineId, row?.region ?? location, "", [...remaining, ...own], marks); return false; }
      marks[location] = Math.max(marks[location] ?? 0, sequence);
      // A regional broker confirms its local replacement; until then a retry can fence it explicitly.
      this.#save(machineId, location, generation, location === "legacy" ? [] : [{ location, keep: generation, reason: "host_replaced" }], marks);
      return true;
    });
  }

  /** The claimant's broker replaced every other local host of the machine. */
  confirm(machineId: string, location: HandRelayLocation, generation: string): Promise<void> {
    return this.#serial(async () => {
      const row = this.#row(machineId);
      if (!row || row.region !== location || row.generation !== generation) return;
      const pending = (JSON.parse(row.pending_fences) as Fence[]).filter(fence => !(fence.location === location && fence.keep === generation));
      this.#save(machineId, row.region, row.generation, pending, JSON.parse(row.watermarks));
    });
  }

  /** Withdraw a machine's screen authority and fence every known location. */
  revoke(machineId: string, extra: readonly HandRelayLocation[] = []): Promise<boolean> {
    return this.#serial(async () => {
      const row = this.#row(machineId);
      if (!row && !extra.length) return true;
      const marks: Record<string, number> = row ? JSON.parse(row.watermarks) : {};
      const fences = RegionalScreenAuthority.#unique([
        ...(row ? JSON.parse(row.pending_fences) as Fence[] : []).map(fence => ({ location: fence.location, reason: "publisher_revoked" as const })),
        ...(row ? [{ location: row.region as HandRelayLocation, reason: "publisher_revoked" as const }] : []),
        ...extra.map(location => ({ location, reason: "publisher_revoked" as const })),
      ]);
      const region = row?.region ?? "legacy";
      this.#save(machineId, region, "", fences, marks);
      const remaining = await this.#drain(machineId, fences, "publisher_revoked", marks);
      this.#save(machineId, region, "", remaining, marks);
      return remaining.length === 0;
    });
  }

  /** Authority rows, including withdrawn ones (empty generation lists nothing). */
  hosts(): Map<string, ScreenAuthority> {
    return new Map(this.storage.sql.exec<Row>("SELECT * FROM regional_screen_hosts").toArray()
      .map(row => [row.machine_id, { region: row.region as HandRelayLocation, generation: row.generation }]));
  }

  pending(): boolean {
    return this.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM regional_screen_hosts WHERE pending_fences<>'[]'").toArray()[0]!.n > 0;
  }

  /** Retry retained fences outside any claim (listing), never against a waiting relay. */
  retryPending(): Promise<void> {
    return this.#serial(async () => {
      for (const row of this.storage.sql.exec<Row>("SELECT * FROM regional_screen_hosts WHERE pending_fences<>'[]'").toArray()) {
        const marks: Record<string, number> = JSON.parse(row.watermarks);
        const remaining = await this.#drain(row.machine_id, JSON.parse(row.pending_fences) as Fence[],
          row.generation ? "host_replaced" : "publisher_revoked", marks);
        this.#save(row.machine_id, row.region, row.generation, remaining, marks);
      }
    });
  }
}

/** Whether a broker-local surface may be listed under the owner's authority. */
export function screenAuthorized(authority: ReadonlyMap<string, ScreenAuthority>, location: HandRelayLocation, machineId: string, generation: string): boolean {
  const current = authority.get(machineId);
  // Legacy publications that predate authority rows stay visible.
  if (!current) return location === "legacy";
  return current.region === location && current.generation === generation;
}
