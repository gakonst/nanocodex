import { DurableObject } from "cloudflare:workers";
import type { Principal } from "./account-auth";

/** Private gateway bindings only; neither object has a public forwarding route. */
export interface InferenceKeysEnv {
  NANOCODEX_INFERENCE_KEYS: DurableObjectNamespace<InferenceKey>;
  NANOCODEX_INFERENCE_ACCOUNTS: DurableObjectNamespace<InferenceAccount>;
}

export type InferenceKeyLimits = Readonly<{
  requestsPerDay: number;
  requestsPerMinute: number;
  maxOutputTokens: number;
}>;
export type InferenceKeyMetadata = Readonly<{
  /** Immutable credential scope; never supplied by callers. */
  scope: "inference";
  id: string;
  label: string;
  createdAt: number;
  expiresAt: number | null;
  limits: InferenceKeyLimits;
  revokedAt: number | null;
}>;
export type InferenceKeyContext = Readonly<{
  kind: "inference_key";
  scope: "inference";
  id: string;
  /** Owning user for attribution only; this context is not an account principal. */
  userId: string;
  limits: InferenceKeyLimits;
}>;
type KeyRecord = InferenceKeyMetadata & { userId: string; digest: string };
type Usage = { day: number; dayCount: number; minute: number; minuteCount: number };

export const DEFAULT_INFERENCE_KEY_LIMITS: InferenceKeyLimits = Object.freeze({
  requestsPerDay: 100,
  requestsPerMinute: 10,
  maxOutputTokens: 4096,
});
const KEY_ID = /^[A-Za-z0-9_-]{43}$/;
const TOKEN = /^nci_live_([A-Za-z0-9_-]{43})_([A-Za-z0-9_-]{43})$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const DIGEST = /^[0-9a-f]{64}$/;
const PRIVATE_ORIGIN = "https://inference.internal";

function json(value: unknown, status = 200, headers?: HeadersInit): Response {
  const output = new Headers(headers);
  output.set("cache-control", "no-store");
  output.set("x-content-type-options", "nosniff");
  return Response.json(value, { status, headers: output });
}
function error(name: string, status: number): Response { return json({ error: name }, status); }
function object(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function integer(value: unknown, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0 && value <= max;
}
function limits(value: unknown): value is InferenceKeyLimits {
  return object(value) && Object.keys(value).length === 3
    && integer(value.requestsPerDay, 1_000_000)
    && integer(value.requestsPerMinute, 10_000)
    && integer(value.maxOutputTokens, 4096);
}
function metadata(value: unknown): value is InferenceKeyMetadata {
  return object(value) && value.scope === "inference"
    && typeof value.id === "string" && KEY_ID.test(value.id)
    && typeof value.label === "string" && value.label.length > 0 && value.label.length <= 120
    && integer(value.createdAt, Number.MAX_SAFE_INTEGER)
    && (value.expiresAt === null || integer(value.expiresAt, Number.MAX_SAFE_INTEGER))
    && (value.revokedAt === null || integer(value.revokedAt, Number.MAX_SAFE_INTEGER))
    && limits(value.limits);
}
/** Explicit projection: never return a stored record (which contains the digest). */
function publicMetadata(value: InferenceKeyMetadata): InferenceKeyMetadata {
  return {
    scope: value.scope, id: value.id, label: value.label, createdAt: value.createdAt, expiresAt: value.expiresAt,
    limits: { ...value.limits }, revokedAt: value.revokedAt,
  };
}
async function readJson(request: Request): Promise<Record<string, unknown> | Response> {
  if (request.headers.get("content-type")?.split(";")[0]?.trim().toLowerCase() !== "application/json") {
    return error("expected_json", 415);
  }
  if (!request.body) return error("invalid_json", 400);
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 4096) {
        await reader.cancel();
        return error("payload_too_large", 413);
      }
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    const value: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes));
    return object(value) ? value : error("invalid_json", 400);
  } catch { return error("invalid_json", 400); }
  finally { reader.releaseLock(); }
}
function randomPart(): string {
  return btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32))))
    .replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
async function digestToken(token: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token));
  return [...new Uint8Array(digest)].map(byte => byte.toString(16).padStart(2, "0")).join("");
}
function sameDigest(left: string, right: string): boolean {
  if (!DIGEST.test(left) || !DIGEST.test(right)) return false;
  let difference = 0;
  for (let i = 0; i < 64; i++) difference |= left.charCodeAt(i) ^ right.charCodeAt(i);
  return difference === 0;
}
function internalRequest(path: string, method: string, body?: unknown): Request {
  return new Request(`${PRIVATE_ORIGIN}${path}`, {
    method,
    ...(body === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
  });
}

export class InferenceKey extends DurableObject<InferenceKeysEnv> {
  async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/record" && request.method === "PUT") {
      const body = await readJson(request);
      if (body instanceof Response) return body;
      const { userId, digest } = body;
      if (!metadata(body) || body.revokedAt !== null || typeof userId !== "string"
        || !UUID.test(userId) || typeof digest !== "string" || !DIGEST.test(digest)) {
        return error("invalid_key_record", 400);
      }
      const record: KeyRecord = { ...publicMetadata(body), userId, digest };
      return this.ctx.storage.transaction(async storage => {
        if (await storage.get("record")) return error("conflict", 409);
        await storage.put("record", record);
        return json(publicMetadata(record), 201);
      });
    }
    if (path === "/record" && request.method === "GET") {
      const record = await this.ctx.storage.get<KeyRecord>("record");
      return record && metadata(record) ? json(publicMetadata(record)) : error("not_found", 404);
    }
    if (path === "/record" && request.method === "DELETE") {
      return this.ctx.storage.transaction(async storage => {
        const record = await storage.get<KeyRecord | { revokedAt: number }>("record");
        // Even a missing record needs a tombstone: issuance may still be in flight.
        await storage.put("record", { ...record, revokedAt: record?.revokedAt ?? Date.now() });
        return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
      });
    }
    if ((path === "/authorize" || path === "/reserve") && request.method === "POST") {
      const body = await readJson(request);
      if (body instanceof Response) return body;
      if (typeof body.digest !== "string" || !DIGEST.test(body.digest)) return error("unauthorized", 401);
      const suppliedDigest = body.digest;
      return this.ctx.storage.transaction(async storage => {
        const record = await storage.get<KeyRecord>("record");
        const now = Date.now();
        if (!record || !metadata(record) || record.revokedAt !== null
          || (record.expiresAt !== null && record.expiresAt <= now)
          || !sameDigest(record.digest, suppliedDigest)) return error("unauthorized", 401);
        if (path === "/reserve") {
          // Fixed UTC day/minute windows; rejected attempts do not consume a slot.
          const day = Math.floor(now / 86_400_000);
          const minute = Math.floor(now / 60_000);
          const previous = await storage.get<Usage>("usage");
          const usage: Usage = {
            day, minute, dayCount: previous?.day === day ? previous.dayCount : 0,
            minuteCount: previous?.minute === minute ? previous.minuteCount : 0,
          };
          const daily = usage.dayCount >= record.limits.requestsPerDay;
          const perMinute = usage.minuteCount >= record.limits.requestsPerMinute;
          if (daily || perMinute) {
            const retryAt = daily ? (day + 1) * 86_400_000 : (minute + 1) * 60_000;
            return json({ error: "rate_limit_exceeded" }, 429, { "retry-after": String(Math.ceil((retryAt - now) / 1000)) });
          }
          usage.dayCount++;
          usage.minuteCount++;
          await storage.put("usage", usage);
        }
        return json({ kind: "inference_key", scope: "inference", id: record.id, userId: record.userId, limits: record.limits } satisfies InferenceKeyContext);
      });
    }
    return error("not_found", 404);
  }
}

/** An owner-scoped registry of metadata and issuance operation IDs; never secrets or digests. */
export class InferenceAccount extends DurableObject<InferenceKeysEnv> {
  async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/keys" && request.method === "GET") {
      const records = await this.ctx.storage.list<InferenceKeyMetadata>({ prefix: "key:" });
      return json({ data: [...records.values()].map(publicMetadata) });
    }
    if (path === "/keys" && request.method === "POST") {
      const body = await readJson(request);
      if (body instanceof Response) return body;
      if (!metadata(body.key) || body.key.revokedAt !== null || typeof body.operation_id !== "string"
        || !UUID.test(body.operation_id)) return error("invalid_key_metadata", 400);
      const key = publicMetadata(body.key);
      const operation = `operation:${body.operation_id.toLowerCase()}`;
      return this.ctx.storage.transaction(async storage => {
        const existingId = await storage.get<string>(operation);
        if (existingId) {
          const existing = await storage.get<InferenceKeyMetadata>(`key:${existingId}`);
          return json({ error: "already_issued", key: existing && publicMetadata(existing) }, 409);
        }
        if (await storage.get(`key:${key.id}`)) return error("conflict", 409);
        await storage.put({ [operation]: key.id, [`key:${key.id}`]: key });
        return json({ key }, 201);
      });
    }
    const match = path.match(/^\/keys\/([A-Za-z0-9_-]{43})$/);
    if (match && request.method === "GET") {
      const key = await this.ctx.storage.get<InferenceKeyMetadata>(`key:${match[1]}`);
      return key ? json({ key: publicMetadata(key) }) : error("not_found", 404);
    }
    if (match && request.method === "DELETE") {
      return this.ctx.storage.transaction(async storage => {
        const id = `key:${match[1]}`;
        const key = await storage.get<InferenceKeyMetadata>(id);
        if (!key) return error("not_found", 404);
        const revoked = { ...publicMetadata(key), revokedAt: key.revokedAt ?? Date.now() };
        await storage.put(id, revoked);
        return json({ key: revoked });
      });
    }
    return error("not_found", 404);
  }
}

/** Authentication is deliberately separate from the full account principal path. */
export async function authorizeInferenceKey(
  request: Request,
  env: InferenceKeysEnv,
  options: { reserve?: boolean } = {},
): Promise<InferenceKeyContext | Response> {
  const authorization = request.headers.get("authorization");
  // Exact bearer value only. Cookies, query parameters, ncx keys and extra values never qualify.
  if (!authorization?.startsWith("Bearer ")) return error("unauthorized", 401);
  const token = authorization.slice(7);
  const match = TOKEN.exec(token);
  if (!match) return error("unauthorized", 401);
  try {
    const result = await env.NANOCODEX_INFERENCE_KEYS.getByName(match[1]!).fetch(internalRequest(
      options.reserve ? "/reserve" : "/authorize", "POST", { digest: await digestToken(token) },
    ));
    if (!result.ok) return result;
    return await result.json<InferenceKeyContext>();
  } catch { return error("inference_auth_unavailable", 503); }
}

/** Caller authenticates the full account first. This function never authenticates inference keys. */
export async function routeInferenceKeys(
  request: Request,
  env: InferenceKeysEnv,
  url: URL,
  principal: Principal | null | undefined,
): Promise<Response | undefined> {
  const collection = url.pathname === "/v1/inference/keys";
  const match = url.pathname.match(/^\/v1\/inference\/keys\/([A-Za-z0-9_-]{43})$/);
  if (!collection && !match) return undefined;
  if (/\bnci_/i.test(request.headers.get("authorization") ?? "")) return error("forbidden", 403);
  if (!principal) return error("unauthorized", 401);
  if ((principal.kind !== "account_session" && principal.kind !== "api_key") || principal.connectGrant) {
    return error("forbidden", 403);
  }
  if (!(collection ? ["GET", "POST"] : ["DELETE"]).includes(request.method)) return error("method_not_allowed", 405);
  if (!principal.capabilities.includes(request.method === "GET" ? "api_keys:read" : "api_keys:write")) {
    return error("forbidden", 403);
  }
  if (request.method !== "GET" && principal.kind === "account_session" && request.headers.get("origin") !== url.origin) {
    return error("forbidden_origin", 403);
  }
  try {
    const registry = env.NANOCODEX_INFERENCE_ACCOUNTS.getByName(principal.userId);
    if (request.method === "GET") return await registry.fetch(internalRequest("/keys", "GET"));
    if (request.method === "DELETE") {
      const id = match![1]!;
      // Check ownership before touching the globally addressed key object.
      const owned = await registry.fetch(internalRequest(`/keys/${id}`, "GET"));
      if (!owned.ok) return owned;
      const revoked = await env.NANOCODEX_INFERENCE_KEYS.getByName(id).fetch(internalRequest("/record", "DELETE"));
      if (!revoked.ok) return error("inference_key_write_unavailable", 503);
      const updated = await registry.fetch(internalRequest(`/keys/${id}`, "DELETE"));
      return updated.ok ? new Response(null, { status: 204, headers: { "cache-control": "no-store" } }) : updated;
    }
    const body = await readJson(request);
    if (body instanceof Response) return body;
    if (Object.keys(body).some(key => !["label", "expires_at", "limits", "operation_id"].includes(key))) return error("invalid_key_options", 400);
    const label = body.label === undefined ? "Inference key" : typeof body.label === "string" ? body.label.trim() : "";
    const createdAt = Date.now();
    const expiresAt = body.expires_at === undefined ? createdAt + 30 * 86_400_000 : body.expires_at;
    const keyLimits = object(body.limits) ? { ...DEFAULT_INFERENCE_KEY_LIMITS, ...body.limits } : DEFAULT_INFERENCE_KEY_LIMITS;
    const operationId = body.operation_id ?? crypto.randomUUID();
    if (!label || label.length > 120 || (!integer(expiresAt, Number.MAX_SAFE_INTEGER) || expiresAt <= createdAt)
      || (body.limits !== undefined && !object(body.limits)) || !limits(keyLimits)
      || typeof operationId !== "string" || !UUID.test(operationId)) return error("invalid_key_options", 400);
    const id = randomPart();
    const token = `nci_live_${id}_${randomPart()}`;
    const key: InferenceKeyMetadata = { scope: "inference", id, label, createdAt, expiresAt, limits: keyLimits, revokedAt: null };
    // Claim first, once. Any ambiguous failure permanently consumes this operation ID;
    // a retry returns metadata only and cannot mint a second bearer token.
    const claimed = await registry.fetch(internalRequest("/keys", "POST", { key, operation_id: operationId }));
    if (!claimed.ok) return claimed;
    const stored = await env.NANOCODEX_INFERENCE_KEYS.getByName(id).fetch(internalRequest("/record", "PUT", {
      ...key, userId: principal.userId, digest: await digestToken(token),
    } satisfies KeyRecord));
    if (!stored.ok) return error("inference_key_write_unavailable", 503);
    return json({ api_key: token, key }, 201);
  } catch {
    // Never retry a write here: its outcome may be unknown. Never echo tokens or backend errors.
    return error("inference_key_write_unavailable", 503);
  }
}
