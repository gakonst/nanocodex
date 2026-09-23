import type { CloudflareAccountDiscoveryOptions, CloudflareAccountDiscoveryResult, CloudflareAccountMetadataComponent } from "nanocodex/cloudflare/egress";

export const METADATA_CACHE_NAME = "nanocodex-account-discovery-v1";
export const METADATA_TTL_MS = 15 * 60_000;
const MAX_BYTES = 256 * 1024;
const encoder = new TextEncoder();

export function validDiscoveryOptions(value: unknown): value is CloudflareAccountDiscoveryOptions {
  return record(value) && typeof value.authorityKey === "string" && value.authorityKey.length > 0
    && encoder.encode(value.authorityKey).byteLength <= 4096
    && (value.reload === undefined || typeof value.reload === "boolean")
    && Object.keys(value).every(key => key === "authorityKey" || key === "reload");
}

/** DO IDs include their namespace identity; names alone do not partition bindings. */
export async function metadataCacheKey(ownerId: string, component: CloudflareAccountMetadataComponent, authorityKey: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(JSON.stringify([1, ownerId, component, authorityKey])));
  const hash = Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, "0")).join("");
  // An internal key only: no HTTP route serves this named cache.
  return `https://account-discovery.internal/v1/${hash}`;
}

/** Discovery only. Reload is backend-fresh, not a global purge or a CAS write. */
export async function cachedAccountMetadata(
  ownerId: string,
  component: CloudflareAccountMetadataComponent,
  options: CloudflareAccountDiscoveryOptions,
  live: () => Promise<Readonly<{ status: number; data: unknown }>>,
  ctx: Pick<ExecutionContext, "waitUntil">,
): Promise<CloudflareAccountDiscoveryResult> {
  const observation = {
    type: "egress.metadata_cache", component,
    cache_state: (options.reload ? "reload" : "miss") as "hit" | "miss" | "reload" | "unavailable",
    cache_read_ms: 0, backend_ms: 0, write_scheduled: false, remaining_ttl_ms: 0,
  };
  let originalExpiresAt = 0;
  try {
    let cache: Cache | undefined;
    let key: string | undefined;
    const readStarted = Date.now();
    try {
      key = await metadataCacheKey(ownerId, component, options.authorityKey);
      cache = await caches.open(METADATA_CACHE_NAME);
      if (!options.reload) {
        const response = await cache.match(key);
        if (response) {
          if (response.status !== 200) { await response.body?.cancel(); throw new Error("invalid cache status"); }
          const value: unknown = JSON.parse(await boundedText(response));
          if (record(value) && exact(value, ["schema", "status", "expiresAt", "data"])
            && value.schema === 1 && value.status === 200 && Number.isSafeInteger(value.expiresAt)
            && Number(value.expiresAt) > Date.now() && Number(value.expiresAt) <= Date.now() + METADATA_TTL_MS
            && safeMetadata(component, value.data)) {
            observation.cache_state = "hit";
            originalExpiresAt = Number(value.expiresAt);
            return value as CloudflareAccountDiscoveryResult;
          }
        }
      }
    } catch {
      observation.cache_state = "unavailable";
      // Cache unavailability or corrupt entries must not break live reads.
    } finally { observation.cache_read_ms = boundedMilliseconds(Date.now() - readStarted); }
    const backendStarted = Date.now();
    const expiresAt = backendStarted + METADATA_TTL_MS;
    let result: Awaited<ReturnType<typeof live>>;
    try { result = await live(); } // Backend errors propagate; never retry or cache them.
    finally { observation.backend_ms = boundedMilliseconds(Date.now() - backendStarted); }
    if (result.status === 200) originalExpiresAt = expiresAt;
    const envelope: CloudflareAccountDiscoveryResult = { schema: 1, ...result, expiresAt };
    if (cache && key && result.status === 200 && safeMetadata(component, result.data)) {
      try {
        const body = JSON.stringify(envelope);
        const remainingSeconds = Math.floor((expiresAt - Date.now()) / 1000);
        if (encoder.encode(body).byteLength <= MAX_BYTES && remainingSeconds > 0) {
          // A late write can win, but retains its original expiry in both layers.
          // Keep optional storage off the response path, including rejected puts.
          const write = cache.put(key, new Response(body, { headers: {
            "content-type": "application/json", "cache-control": `max-age=${remainingSeconds}`,
            expires: new Date(expiresAt).toUTCString(),
          } })).catch(() => { /* The caller already has fresh metadata. */ });
          ctx.waitUntil(write);
          observation.write_scheduled = true;
        }
      } catch {
        observation.cache_state = "unavailable";
        // Cache writes are optional; the caller already has fresh metadata.
      }
    }
    return envelope;
  } finally {
    // Fixed fields only: never include identities, cache keys, authority, data or errors.
    try {
      observation.remaining_ttl_ms = Math.min(METADATA_TTL_MS, boundedMilliseconds(originalExpiresAt - Date.now()));
      console.log(observation);
    } catch { /* Diagnostics must never fail metadata discovery. */ }
  }
}

function boundedMilliseconds(value: number): number {
  return Number.isFinite(value) ? Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, Math.round(value))) : 0;
}

async function boundedText(response: Response): Promise<string> {
  if (!response.body) throw new Error("empty metadata cache entry");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > MAX_BYTES) throw new Error("oversized metadata cache entry");
      chunks.push(value);
    }
  } finally { await reader.cancel(); }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes);
}

type Check = (value: unknown) => boolean;
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const exact = (value: Record<string, unknown>, keys: readonly string[]) => Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
const text = (limit: number): Check => value => typeof value === "string" && value.length > 0 && encoder.encode(value).byteLength <= limit && !/[\u0000-\u001f\u007f]/.test(value);
const list = (limit: number, check: Check): Check => value => Array.isArray(value) && value.length <= limit && value.every(check);
const shape = (fields: Record<string, Check>): Check => value => record(value) && exact(value, Object.keys(fields)) && Object.entries(fields).every(([key, check]) => check(value[key]));
const id: Check = value => typeof value === "string" && /^[A-Za-z0-9_-]{22,64}$/.test(value);
const timestamp: Check = value => Number.isSafeInteger(value) && Number(value) >= 0;
const capability = text(64);
const connection = shape({ id, label: text(512), account_id: text(512), capabilities: list(32, capability) });
const connector = shape({ connected: value => typeof value === "boolean", connections: list(100, connection) });
const mcp = shape({ id, name: text(512), status: text(64) });

/** Exact schemas prevent new credential-bearing fields from silently becoming cacheable. */
export function safeMetadata(component: CloudflareAccountMetadataComponent, value: unknown): boolean {
  if (component === "catalog") {
    return record(value) && exact(value, ["connectors", "mcp_connections"])
      && record(value.connectors) && Object.keys(value.connectors).length <= 32
      && Object.entries(value.connectors).every(([key, status]) => /^[a-z][a-z0-9_]{0,63}$/.test(key) && connector(status))
      && list(256, mcp)(value.mcp_connections);
  }
  return list(100, vaultEntry)(value);
}

function vaultEntry(value: unknown): boolean {
  if (!record(value) || typeof value.kind !== "string") return false;
  const fields: Record<string, Check> = { id, kind: text(16), name: text(120), created_at: timestamp };
  switch (value.kind) {
    case "api_key": break;
    case "login":
      fields.username = text(512);
      if (Object.hasOwn(value, "browser_origin")) fields.browser_origin = origin;
      break;
    case "card": fields.last4 = value => typeof value === "string" && /^[0-9]{4}$/.test(value); break;
    case "address":
      Object.assign(fields, { address_line_1: text(256), city: text(120), state: text(120), zip: text(32), country: text(120) });
      if (Object.hasOwn(value, "address_line_2")) fields.address_line_2 = text(256);
      break;
    case "phone": fields.phone_number = text(64); break;
    default: return false;
  }
  return shape(fields)(value);
}
function origin(value: unknown): boolean {
  if (!text(2048)(value)) return false;
  try { const url = new URL(value as string); return url.protocol === "https:" && url.origin === value; } catch { return false; }
}
