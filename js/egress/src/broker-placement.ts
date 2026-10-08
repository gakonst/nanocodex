/**
 * Placement of the canonical per-user credential broker.
 *
 * The legacy object is named by the bare user id and was pinned wherever it
 * was first created. A home object is named by region and user id and is
 * created with that region's location hint, so the canonical broker (sealed
 * credentials, Vault, lease registry) lives next to the user's model traffic.
 *
 * Exactly one object per user is "active". Every other object is "moved"
 * (a fail-closed tombstone that names its successor) or "empty" (a home that
 * has never been adopted). Non-active objects refuse every operation before
 * any side effect, so a caller may safely re-locate and retry once.
 */
export const PLACEMENT_REGIONS: ReadonlySet<string> = new Set(["wnam", "enam", "sam", "weur", "eeur", "apac", "oc"]);
/** "~" and "/" are outside the user-id alphabet, so names never collide. */
export const HOME_BROKER_PREFIX = "~home/v1/";
const USER_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
export const BROKER_MOVED = "broker_moved";
export const BROKER_MOVED_HEADER = "x-nanocodex-broker-moved";
/** Minimum residency of an adopted home before another region may claim it. */
export const REHOME_COOLDOWN_MS = 15 * 60_000;
/** Bounded directory walk; each re-home adds at most one link. */
export const MAX_PLACEMENT_HOPS = 8;

export type BrokerName =
  | Readonly<{ kind: "legacy"; userId: string; name: string }>
  | Readonly<{ kind: "home"; userId: string; region: DurableObjectLocationHint; name: string }>;

export function homeBrokerName(region: string, userId: string): string {
  if (!PLACEMENT_REGIONS.has(region) || !USER_ID.test(userId)) throw new TypeError("invalid broker placement");
  return `${HOME_BROKER_PREFIX}${region}/${userId}`;
}

export function parseBrokerName(name: unknown): BrokerName | undefined {
  if (typeof name !== "string") return undefined;
  if (USER_ID.test(name)) return { kind: "legacy", userId: name, name };
  if (!name.startsWith(HOME_BROKER_PREFIX)) return undefined;
  const rest = name.slice(HOME_BROKER_PREFIX.length);
  const slash = rest.indexOf("/");
  if (slash < 0) return undefined;
  const region = rest.slice(0, slash);
  const userId = rest.slice(slash + 1);
  if (!PLACEMENT_REGIONS.has(region) || !USER_ID.test(userId)) return undefined;
  return { kind: "home", userId, region: region as DurableObjectLocationHint, name };
}

export type PlacementState =
  | Readonly<{ state: "active"; activatedAt: number; source?: string }>
  | Readonly<{ state: "moved"; target: string; movedAt: number; released?: boolean }>
  | Readonly<{ state: "empty" }>;
export type ReleaseResult =
  | Readonly<{ status: "exported"; rows: [string, unknown][] }>
  | Readonly<{ status: "moved"; target: string }>
  | Readonly<{ status: "empty" | "invalid" }>;

export type AdoptResult =
  | Readonly<{ status: "active"; migrated: boolean; source?: string }>
  | Readonly<{ status: "redirect"; target: string }>
  | Readonly<{ status: "unavailable" | "invalid" }>;

/** Thrown (across RPC) by a non-active broker before any work. */
export class BrokerMovedError extends Error {
  constructor(readonly target?: string) { super(target ? `${BROKER_MOVED}:${target}` : BROKER_MOVED); }
}

/** undefined: not a placement refusal. Otherwise the named successor, if any. */
export function brokerMovedTarget(error: unknown): { target?: string } | undefined {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : undefined;
  if (!message) return undefined;
  const index = message.indexOf(BROKER_MOVED);
  if (index < 0) return undefined;
  const rest = message.slice(index + BROKER_MOVED.length);
  if (!rest.startsWith(":")) return {};
  const target = rest.slice(1).trim();
  return parseBrokerName(target) ? { target } : {};
}

export function validPlacement(value: unknown): PlacementState | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const row = value as Record<string, unknown>;
  if (row.state === "active" && typeof row.activatedAt === "number" && Number.isFinite(row.activatedAt)
    && (row.source === undefined || parseBrokerName(row.source))) {
    return { state: "active", activatedAt: row.activatedAt, ...(typeof row.source === "string" ? { source: row.source } : {}) };
  }
  if (row.state === "moved" && parseBrokerName(row.target) && typeof row.movedAt === "number"
    && (row.released === undefined || typeof row.released === "boolean")) {
    return { state: "moved", target: row.target as string, movedAt: row.movedAt, ...(row.released ? { released: true } : {}) };
  }
  return undefined;
}
