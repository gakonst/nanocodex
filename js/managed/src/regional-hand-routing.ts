import type { HostedMachine } from "nanocodex-tools/hosted";
import type { AccountHostedTools } from "./account-hosted-tools";
import type { RegionalHandRelay } from "./regional-hand-relay";

/** Finite shard keys, never caller-controlled object names or placement hints. */
export const HAND_RELAY_REGIONS = ["wnam", "enam", "weur", "eeur", "apac", "oc", "sam", "afr", "me"] as const;
export type HandRelayRegion = typeof HAND_RELAY_REGIONS[number];
export type HandRelayLocation = HandRelayRegion | "legacy";
export type RegionalHandEnv = {
  NANOCODEX_ACCOUNT_TOOLS?: DurableObjectNamespace<AccountHostedTools>;
  NANOCODEX_HAND_RELAYS?: DurableObjectNamespace<RegionalHandRelay>;
};
export const HAND_RELAY_REGION_HEADER = "x-nanocodex-hand-relay-region";
export const HAND_MACHINE_HEADER = "x-nanocodex-hand-machine-id";
export const HAND_RUNTIME_HEADER = "x-nanocodex-hand-runtime-id";
export const HAND_OWNER_HEADER = "x-nanocodex-owner-id";
export function isHandRelayRegion(value: unknown): value is HandRelayRegion {
  return typeof value === "string" && (HAND_RELAY_REGIONS as readonly string[]).includes(value);
}
export function handRelayName(owner: string, region: HandRelayRegion): string {
  return `${owner}:hand-relay:v1:${region}`;
}

/** Cloudflare's ingress metadata is trusted; HTTP headers and query parameters are not. */
export function handRelayRegion(request: Request): HandRelayRegion | undefined {
  const cf = request.cf;
  if (!cf) return undefined;
  const country = cf.country, continent = cf.continent;
  if (["AE", "BH", "IL", "IQ", "JO", "KW", "LB", "OM", "QA", "SA", "TR"].includes(String(country))) return "me";
  if (continent === "NA") return Number(cf.longitude ?? -80) < -100 ? "wnam" : "enam";
  if (continent === "SA") return "sam";
  if (continent === "AF") return "afr";
  if (continent === "OC") return "oc";
  if (continent === "AS") return "apac";
  if (continent === "EU") return Number(cf.longitude ?? 0) >= 20 ? "eeur" : "weur";
  return undefined;
}

export async function routeRegionalToolHost(request: Request, owner: string, env: RegionalHandEnv): Promise<Response> {
  const headers = new Headers(request.headers);
  headers.set(HAND_OWNER_HEADER, owner);
  headers.delete(HAND_RELAY_REGION_HEADER);
  const identity = publisherIdentity(headers);
  if (identity === false) return Response.json({ error: "invalid_publisher_identity" }, { status: 400 });
  let region: HandRelayLocation = "legacy";
  if (identity) {
    const selection = await env.NANOCODEX_ACCOUNT_TOOLS!.getByName(owner).fetch("https://account-tools.internal/regional/select", {
      method: "POST", headers: { [HAND_OWNER_HEADER]: owner, "content-type": "application/json" },
      body: JSON.stringify({ machine_id: identity.machineId, runtime_id: identity.runtimeId,
        region: env.NANOCODEX_HAND_RELAYS
          ? handRelayRegion(request) ?? "legacy" : "legacy" }),
    });
    if (!selection.ok) return selection;
    const value = await selection.json<{ region: HandRelayLocation }>();
    if (value.region !== "legacy" && !isHandRelayRegion(value.region)) throw new Error("invalid Hand placement");
    region = value.region;
  }
  if (region !== "legacy" && !env.NANOCODEX_HAND_RELAYS) return Response.json({ error: "relay_unavailable" }, { status: 503 });
  const target = region !== "legacy"
    ? env.NANOCODEX_HAND_RELAYS!.getByName(handRelayName(owner, region), { locationHint: region })
    : env.NANOCODEX_ACCOUNT_TOOLS!.getByName(owner);
  if (region !== "legacy") headers.set(HAND_RELAY_REGION_HEADER, region);
  return target.fetch(new Request("https://account-tools.internal/tool-host", new Request(request, { headers })));
}

export function publisherIdentity(headers: Headers): { machineId: string; runtimeId: string } | undefined | false {
  const machineId = headers.get(HAND_MACHINE_HEADER), runtimeId = headers.get(HAND_RUNTIME_HEADER);
  if (machineId === null && runtimeId === null) return undefined;
  return validPublisherId(machineId) && validPublisherId(runtimeId) ? { machineId, runtimeId } : false;
}
export function validPublisherId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value);
}
export function relayRouteToken(region: HandRelayRegion, token: string): string {
  return `hand-relay:v1:${region}:${token}`;
}
export function parseRelayRouteToken(token: string): { region: HandRelayRegion; token: string } | undefined {
  const match = /^hand-relay:v1:([^:]+):(.+)$/s.exec(token);
  return match && isHandRelayRegion(match[1]) ? { region: match[1], token: match[2]! } : undefined;
}

export type HandPublication = Readonly<{
  route_id: string;
  publication_id: string;
  region: HandRelayLocation;
  machine: HostedMachine;
  tool_names: readonly string[];
  runtime_id?: string;
}>;
type DirectoryEntry = HandPublication & { pending: boolean; previous: readonly HandPublication[] };

/** Account-owned authority. Publication/upgrade pay coordination cost; calls do not. */
export class RegionalHandDirectory {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS regional_hand_directory (
      machine_id TEXT PRIMARY KEY, publication_json TEXT NOT NULL
    ); CREATE TABLE IF NOT EXISTS regional_hand_placements (
      machine_id TEXT NOT NULL, runtime_id TEXT NOT NULL, region TEXT NOT NULL,
      retired INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(machine_id,runtime_id)
    )`);
  }
  entries(): readonly DirectoryEntry[] {
    return this.storage.sql.exec<{ publication_json: string }>("SELECT publication_json FROM regional_hand_directory ORDER BY machine_id")
      .toArray().map(row => JSON.parse(row.publication_json) as DirectoryEntry);
  }
  placement(machineId: string, runtimeId: string): HandRelayLocation | undefined {
    return this.storage.sql.exec<{ region: HandRelayLocation }>("SELECT region FROM regional_hand_placements WHERE machine_id=? AND runtime_id=?", machineId, runtimeId).toArray()[0]?.region;
  }
  retired(machineId: string, runtimeId: string): boolean {
    return this.storage.sql.exec<{ retired: number }>("SELECT retired FROM regional_hand_placements WHERE machine_id=? AND runtime_id=?", machineId, runtimeId).toArray()[0]?.retired === 1;
  }
  select(machineId: string, runtimeId: string, proposed: HandRelayLocation): HandRelayLocation {
    if (this.retired(machineId, runtimeId)) throw new Error("Hand runtime was superseded");
    const existing = this.placement(machineId, runtimeId);
    if (existing) return existing;
    this.storage.sql.exec("INSERT INTO regional_hand_placements(machine_id,runtime_id,region) VALUES(?,?,?)", machineId, runtimeId, proposed);
    return proposed;
  }
  retire(machineId: string, runtimeId: string): void {
    // Persist the legacy identity even for runtimes predating directory rollout.
    if (!this.placement(machineId, runtimeId)) this.select(machineId, runtimeId, "legacy");
    this.storage.sql.exec("UPDATE regional_hand_placements SET retired=1 WHERE machine_id=? AND runtime_id=?", machineId, runtimeId);
  }
  /**
   * Owner-initiated removal. Unlike retirePublication this does not fence on a
   * specific publication: the owner is discarding the Hand regardless of which
   * runtime currently holds it. Placements stay behind as retired tombstones so
   * a late runtime cannot reclaim the identity after the owner forgot it.
   */
  forget(machineId: string): void {
    for (const row of this.storage.sql.exec<{ runtime_id: string }>(
      "SELECT runtime_id FROM regional_hand_placements WHERE machine_id=?", machineId).toArray()) {
      this.retire(machineId, row.runtime_id);
    }
    this.storage.sql.exec("DELETE FROM regional_hand_directory WHERE machine_id=?", machineId);
  }
  retirePublication(publication: HandPublication): void {
    const current = this.entries().find(entry => entry.machine.id === publication.machine.id);
    if (!current || current.pending || current.publication_id !== publication.publication_id
      || current.region !== publication.region || current.runtime_id !== publication.runtime_id
      || !publication.runtime_id) throw new Error("Hand publication changed");
    this.retire(publication.machine.id, publication.runtime_id);
    this.storage.sql.exec("DELETE FROM regional_hand_directory WHERE machine_id=?", publication.machine.id);
  }
  #save(entry: DirectoryEntry): void {
    this.storage.sql.exec("INSERT INTO regional_hand_directory VALUES(?,?) ON CONFLICT(machine_id) DO UPDATE SET publication_json=excluded.publication_json", entry.machine.id, JSON.stringify(entry));
  }
  async claim(candidate: HandPublication, fence: (entry: HandPublication) => Promise<void>): Promise<void> {
    if (candidate.runtime_id) {
      const retained = this.select(candidate.machine.id, candidate.runtime_id, candidate.region);
      if (retained !== candidate.region) throw new Error("Hand runtime is pinned to another relay");
    }
    const entries = this.entries();
    const old = entries.find(entry => entry.machine.id === candidate.machine.id);
    if (entries.some(entry => entry.machine.id !== candidate.machine.id && entry.tool_names.some(name => candidate.tool_names.includes(name)))) {
      throw new Error("tool name is already exposed by another account Hand");
    }
    // Save unresolved fences before RPC; a successor finishes them after crash.
    const previous = [...(old?.previous ?? []), ...(old ? [old] : [])]
      .filter(entry => entry.region !== candidate.region)
      .map(({ route_id, publication_id, region, machine, tool_names, runtime_id }) => ({ route_id, publication_id, region, machine, tool_names, runtime_id }));
    const unique = [...new Map(previous.map(entry => [`${entry.region}:${entry.publication_id}`, entry])).values()];
    if (unique.length > HAND_RELAY_REGIONS.length + 1) throw new Error("unresolved Hand publication fences");
    if (old?.runtime_id && candidate.runtime_id && old.runtime_id !== candidate.runtime_id) {
      this.storage.sql.exec("UPDATE regional_hand_placements SET retired=1 WHERE machine_id=? AND runtime_id=?", old.machine.id, old.runtime_id);
    }
    const pending: DirectoryEntry = { ...candidate, pending: true, previous: unique };
    this.#save(pending);
    for (const entry of unique) await fence(entry);
    this.#save({ ...candidate, pending: false, previous: [] });
  }
}
