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

type Fence = Readonly<{ location: HandRelayLocation; keep?: string }>;
type Row = { machine_id: string; region: string; generation: string; pending_fences: string };
export type ScreenFenceReason = "host_replaced" | "publisher_revoked";
/** Fence one location; legacy fences run synchronously in the owner (never a self fetch). */
export type ScreenFencer = (location: HandRelayLocation, machineId: string, keep: string | undefined, reason: ScreenFenceReason) => Promise<boolean>;

/** Owner-DO authority for which (location, generation) owns a machine's screen
 * publication. Claims are serialized; failed fences stay pending and retry. */
export class RegionalScreenAuthority {
  #queue: Promise<unknown> = Promise.resolve();
  constructor(private readonly storage: DurableObjectStorage, private readonly fence: ScreenFencer) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_screen_hosts (
      machine_id TEXT PRIMARY KEY, region TEXT NOT NULL, generation TEXT NOT NULL, pending_fences TEXT NOT NULL DEFAULT '[]'
    )`);
  }

  #serial<T>(work: () => Promise<T>): Promise<T> {
    const next = this.#queue.then(work, work);
    this.#queue = next.catch(() => undefined);
    return next;
  }
  #row(machineId: string): Row | undefined {
    return this.storage.sql.exec<Row>("SELECT machine_id,region,generation,pending_fences FROM regional_screen_hosts WHERE machine_id=?", machineId).toArray()[0];
  }
  async #drain(machineId: string, fences: Fence[], reason: ScreenFenceReason): Promise<Fence[]> {
    const remaining: Fence[] = [];
    for (const fence of fences) {
      let ok = false;
      try { ok = await this.fence(fence.location, machineId, fence.keep, reason); } catch { ok = false; }
      if (!ok && !remaining.some(other => other.location === fence.location)) remaining.push(fence);
    }
    return remaining;
  }

  /** Authority is written before the grant; later listings filter stale publications. */
  claim(machineId: string, location: HandRelayLocation, generation: string): Promise<boolean> {
    return this.#serial(async () => {
      const row = this.#row(machineId);
      const fences: Fence[] = row ? JSON.parse(row.pending_fences) as Fence[] : [];
      if (row && row.generation && row.region !== location) fences.push({ location: row.region as HandRelayLocation });
      // Legacy owner hosts predating authority rows are always fenced for a regional claim.
      if (location !== "legacy" && !fences.some(fence => fence.location === "legacy")) fences.push({ location: "legacy" });
      // Same-region ordering: an older claim granted later must not replace this one.
      if (row && row.region === location && location !== "legacy") fences.push({ location, keep: generation });
      const remaining = (await this.#drain(machineId, fences.filter(fence => fence.location !== location || fence.keep === generation), "host_replaced"))
        .filter(fence => fence.location !== location);
      this.storage.sql.exec(`INSERT INTO regional_screen_hosts(machine_id,region,generation,pending_fences) VALUES(?,?,?,?)
        ON CONFLICT(machine_id) DO UPDATE SET region=excluded.region,generation=excluded.generation,pending_fences=excluded.pending_fences`,
      machineId, location, generation, JSON.stringify(remaining));
      return true;
    });
  }

  /** Revoke every regional and legacy publication of a machine. */
  revoke(machineId: string, regions: readonly HandRelayLocation[]): Promise<boolean> {
    return this.#serial(async () => {
      const row = this.#row(machineId);
      const fences: Fence[] = row ? JSON.parse(row.pending_fences) as Fence[] : [];
      for (const location of new Set<HandRelayLocation>(["legacy", ...regions, ...(row ? [row.region as HandRelayLocation] : [])])) {
        if (!fences.some(fence => fence.location === location)) fences.push({ location });
      }
      const remaining = await this.#drain(machineId, fences.map(fence => ({ location: fence.location })), "publisher_revoked");
      if (remaining.length) {
        this.storage.sql.exec(`INSERT INTO regional_screen_hosts(machine_id,region,generation,pending_fences) VALUES(?,'legacy','',?)
          ON CONFLICT(machine_id) DO UPDATE SET generation='',pending_fences=excluded.pending_fences`, machineId, JSON.stringify(remaining));
      } else this.storage.sql.exec("DELETE FROM regional_screen_hosts WHERE machine_id=?", machineId);
      return remaining.length === 0;
    });
  }

  /** Current authority by machine, for filtering regional catalogs. */
  hosts(): Map<string, { region: HandRelayLocation; generation: string }> {
    return new Map(this.storage.sql.exec<Row>("SELECT machine_id,region,generation,pending_fences FROM regional_screen_hosts WHERE generation<>''").toArray()
      .map(row => [row.machine_id, { region: row.region as HandRelayLocation, generation: row.generation }]));
  }

  /** Best-effort retry of retained fences (e.g. while listing screens). */
  retryPending(): Promise<void> {
    return this.#serial(async () => {
      for (const row of this.storage.sql.exec<Row>("SELECT machine_id,region,generation,pending_fences FROM regional_screen_hosts WHERE pending_fences<>'[]'").toArray()) {
        const remaining = await this.#drain(row.machine_id, JSON.parse(row.pending_fences) as Fence[], row.generation ? "host_replaced" : "publisher_revoked");
        if (!row.generation && !remaining.length) this.storage.sql.exec("DELETE FROM regional_screen_hosts WHERE machine_id=?", row.machine_id);
        else this.storage.sql.exec("UPDATE regional_screen_hosts SET pending_fences=? WHERE machine_id=?", JSON.stringify(remaining), row.machine_id);
      }
    });
  }
}
