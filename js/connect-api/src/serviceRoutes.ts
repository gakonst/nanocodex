import type { ServiceCapabilities } from "./servicePolicy.mts";
import { projectVaultEntries, type VaultMetadata } from "./vaultProjection.mjs";

type Fetcher = { fetch(request: Request): Promise<Response> };
type Scope = { id: string; brokerUserId: string; services?: ServiceCapabilities; expiresAt: number };
type Intent = { operationId: string; kind: "purchase" | "release"; numberId?: string; body: string };
type IntentStore = { get<T>(key: string): Promise<T | undefined>; create?(key: string, value: unknown, options?: { ttl?: number }): Promise<boolean> };
const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const reply = (value: unknown, status = 200) => Response.json(value, { status, headers: { "cache-control": "no-store" } });
const failure = (status: number, code: string) => reply({ error: { code, message: code } }, status);

async function readJson(response: Request | Response, limit: number): Promise<Record<string, unknown>> {
  if (response.headers.get("content-type")?.toLowerCase().split(";")[0]?.trim() !== "application/json") throw new Error("invalid_json");
  const reader = response.body?.getReader();
  let size = 0, text = "";
  const decoder = new TextDecoder("utf-8", { fatal: true });
  try {
    if (reader) for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) { await reader.cancel(); throw new Error("too_large"); }
      text += decoder.decode(value, { stream: true });
    }
    const result: unknown = JSON.parse(text + decoder.decode());
    if (!record(result)) throw new Error("invalid_json");
    return result;
  } finally { reader?.releaseLock(); }
}

export function scopedVaultMetadata(entries: readonly VaultMetadata[], scope: Scope): VaultMetadata[] {
  return entries.filter(entry => scope.services?.vault?.ids.includes(entry.id));
}

/** Called only after token resolution, exact app/origin binding and live grant checks. */
export async function serviceRoute(request: Request, broker: Fetcher, scope: Scope, path: string, store: IntentStore): Promise<Response> {
  const url = new URL(request.url);
  if (path === "" && request.method === "GET" && !url.search) {
    const prefix = url.pathname.replace(/\/$/, "");
    return reply({ services: [
      ...(scope.services?.vault ? [{ id: "vault", path: prefix + "/vault", operations: ["list", "get", ...(scope.services.vault.request ? ["request"] : [])], secret_export: false }] : []),
      ...(scope.services?.phone ? [{ id: "phone", path: prefix + "/phone", human_approval_required: true, sms_2fa_compatibility: "provider_and_destination_dependent" }] : []),
    ] });
  }
  const vaultItem = /^vault\/([A-Za-z0-9_-]{22,64})$/.exec(path);
  if ((path === "vault" || vaultItem) && request.method === "GET" && !url.search) {
    if (!scope.services?.vault) return failure(403, "service_not_granted");
    if (vaultItem && !scope.services.vault.ids.includes(vaultItem[1]!)) return failure(403, "vault_item_not_granted");
    if (scope.services.vault.ids.length === 0) return reply({ vault: [] });
    try {
      const result = await broker.fetch(new Request(`https://broker.internal/users/${encodeURIComponent(scope.brokerUserId)}/credentials/vault`, { signal: request.signal }));
      if (!result.ok) throw new Error("unavailable");
      const value = await readJson(result, 256 * 1024);
      // Select before projecting: unapproved entries must never affect the response.
      if (!Array.isArray(value.vault)) throw new Error("invalid_metadata");
      const approved = value.vault.filter(entry => record(entry) && scope.services!.vault!.ids.includes(String(entry.id)));
      const vault = projectVaultEntries(approved);
      if (vaultItem) {
        const entry = vault.find(item => item.id === vaultItem[1]);
        return entry ? reply({ entry }) : failure(404, "vault_entry_unavailable");
      }
      return reply({ vault });
    } catch { return failure(503, "vault_unavailable"); }
  }
  if (path === "vault/request" && request.method === "POST" && !url.search) {
    let value;
    try { value = await readJson(request, 96 * 1024); } catch { return failure(400, "invalid_vault_request"); }
    return serviceVaultRequest(request, broker, scope, value);
  }
  if (path.startsWith("phone/")) return phoneRoute(request, broker, scope, path.slice("phone".length), store);
  return failure(404, "service_route_not_found");
}

/** A second boundary before the owner-only broker: exact item and destination authority. */
export async function serviceVaultRequest(request: Request, broker: Fetcher, scope: Scope, value: Record<string, unknown>): Promise<Response> {
  const vault = scope.services?.vault;
  if (!vault?.request) return failure(403, "service_not_granted");
  if (Object.keys(value).some(key => !["vault_id", "url", "method", "headers", "body", "body_encoding", "signing"].includes(key))
    || typeof value.vault_id !== "string" || !/^[A-Za-z0-9_-]{22,64}$/.test(value.vault_id)
    || typeof value.url !== "string" || value.url.length > 8192) return failure(400, "invalid_vault_request");
  if (!vault.ids.includes(value.vault_id)) return failure(403, "vault_item_not_granted");
  let target: URL;
  try { target = new URL(value.url); } catch { return failure(400, "invalid_vault_request"); }
  if (target.protocol !== "https:" || target.username || target.password || target.hash || !vault.origins.includes(target.origin)) {
    return failure(403, "vault_origin_not_granted");
  }
  const body = JSON.stringify({ ...value, method: value.method ?? "GET", headers: value.headers ?? {} });
  if (new TextEncoder().encode(body).length > 96 * 1024) return failure(400, "invalid_vault_request");
  try {
    const response = await broker.fetch(new Request(`https://vault-egress.internal/v1/users/${encodeURIComponent(scope.brokerUserId)}/request`, {
      method: "POST", headers: { "content-type": "application/json" }, body, redirect: "manual", signal: request.signal,
    }));
    const result = await readJson(response, 4096);
    // The destination body, headers, secrets and generated codes never cross Connect.
    if (response.ok && Number.isInteger(result.status) && Number(result.status) >= 100 && Number(result.status) <= 599
      && result.ok === (Number(result.status) >= 200 && Number(result.status) < 300)) return reply({ status: result.status, ok: result.ok });
    if (!response.ok && typeof result.error === "string" && VAULT_ERRORS.has(result.error)) {
      return failure(response.status, result.error);
    }
  } catch { /* An uncertain mutation must never be retried. */ }
  return failure(502, "vault_request_outcome_unknown");
}
const VAULT_ERRORS = new Set(["invalid_vault_request", "invalid_vault_placeholder", "vault_secret_placeholder_required",
  "vault_entry_kind_mismatch", "vault_entry_unavailable", "vault_broker_unavailable", "invalid_vault_entry_response",
  "vault_destination_denied", "vault_header_denied", "vault_raw_credential_denied", "vault_request_too_large",
  "invalid_vault_signing", "vault_signing_failed", "vault_signing_key_invalid", "vault_signing_kind_mismatch",
  "invalid_vault_signing_placeholder", "vault_totp_origin_mismatch", "vault_totp_placeholder_required"]);

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
async function phoneRoute(request: Request, broker: Fetcher, scope: Scope, tail: string, store: IntentStore): Promise<Response> {
  const phone = scope.services?.phone;
  if (!phone) return failure(403, "service_not_granted");
  const url = new URL(request.url);
  const number = /^\/numbers\/([0-9a-f-]+)(\/messages)?$/.exec(tail);
  const poll = /^\/requests\/([0-9a-f-]+)$/.exec(tail);
  const available = tail === "/numbers/available" && request.method === "GET";
  const list = tail === "/numbers" && request.method === "GET";
  const read = request.method === "GET" && (list || !!number);
  const release = request.method === "DELETE" && number && !number[2];
  const provision = request.method === "POST" && tail === "/numbers";
  if (!read && !release && !provision && !available && !(poll && request.method === "GET")) return failure(404, "service_route_not_found");
  if (read && !phone.read || release && !phone.release || (provision || available) && !phone.provision) return failure(403, "service_not_granted");
  if (number && (!UUID.test(number[1]!) || !phone.numberIds.includes(number[1]!))) return failure(403, "phone_number_not_granted");
  const queryKeys = available ? ["country", "area_code", "limit"] : number?.[2] === "/messages" ? ["limit", "cursor"] : [];
  if ([...url.searchParams.keys()].some(key => !queryKeys.includes(key) || url.searchParams.getAll(key).length !== 1)) return failure(400, "invalid_phone_request");
  let body: string | undefined;
  let operationId: string | undefined;
  let intent: Intent | undefined;
  if (provision || release) {
    let value;
    try { value = await readJson(request, 16 * 1024); } catch { return failure(400, "invalid_phone_request"); }
    if (typeof value.operation_id !== "string" || !UUID.test(value.operation_id)
      || Object.keys(value).some(key => !(provision ? ["operation_id", "phone_number", "country"] : ["operation_id"]).includes(key))
      || provision && (value.country !== "US" || typeof value.phone_number !== "string" || !/^\+1[2-9][0-9]{9}$/.test(value.phone_number))) return failure(400, "invalid_phone_request");
    // Derive an unguessable owner-side namespace, so a caller cannot reuse an
    // owner operation UUID to read or modify an intent created outside this grant.
    operationId = await phoneOperationId(scope.id, value.operation_id);
    body = JSON.stringify({ operation_id: operationId, ...(provision ? { phone_number: value.phone_number, country: value.country } : {}) });
    intent = { operationId: value.operation_id, kind: provision ? "purchase" : "release", ...(number ? { numberId: number[1] } : {}), body };
    const key = `service-phone-intent:${scope.id}:${operationId}`;
    if (!store.create) return failure(503, "phone_intent_unavailable");
    const created = await store.create(key, intent, { ttl: Math.max(1, scope.expiresAt - Math.floor(Date.now() / 1000)) });
    if (!created) {
      const retained = await store.get<Intent>(key);
      if (!retained || retained.body !== body || retained.kind !== intent.kind || retained.numberId !== intent.numberId) return failure(409, "phone_operation_conflict");
    }
  }
  if (poll) {
    if (!UUID.test(poll[1]!)) return failure(404, "phone_request_not_found");
    operationId = await phoneOperationId(scope.id, poll[1]!);
    intent = await store.get<Intent>(`service-phone-intent:${scope.id}:${operationId}`);
    if (!intent) return failure(404, "phone_request_not_found");
    if (intent.kind === "purchase" ? !phone.provision : !phone.release || !phone.numberIds.includes(intent.numberId!)) return failure(403, "service_not_granted");
  }
  try {
    const response = await broker.fetch(new Request(`https://phone-service.internal/v1/users/${encodeURIComponent(scope.brokerUserId)}${poll ? `/requests/${operationId}` : tail}${url.search}`, {
      method: request.method, headers: body === undefined ? {} : { "content-type": "application/json" },
      ...(body === undefined ? {} : { body }), redirect: "manual", signal: request.signal,
    }));
    const value = await readJson(response, 256 * 1024);
    if (!response.ok) return failure(response.status >= 400 && response.status < 600 ? response.status : 502,
      typeof value.error === "string" && PHONE_ERRORS.has(value.error) ? value.error : "phone_service_rejected");
    if (list || available) {
      if (!Array.isArray(value.numbers)) throw new Error("invalid_numbers");
      return reply({ numbers: value.numbers.filter(entry => record(entry) && (available || typeof entry.id === "string" && phone.numberIds.includes(entry.id)))
        .map(entry => project(entry, available ? ["phone_number", "country", "type"] : ["id", "phone_number", "country", "status", "created_at"])) });
    }
    if (number && read) {
      if (number[2]) {
        if (!Array.isArray(value.messages)) throw new Error("invalid_messages");
        return reply({ messages: value.messages.map(entry => project(entry, ["id", "from", "to", "body", "received_at", "expires_at"])),
          ...(typeof value.next_cursor === "string" ? { next_cursor: value.next_cursor } : {}) });
      }
      if (!record(value.number) || value.number.id !== number[1]) throw new Error("invalid_number");
      return reply({ number: project(value.number, ["id", "phone_number", "country", "status", "created_at"]) });
    }
    if (!record(value.request) || value.request.operation_id !== operationId || value.request.kind !== intent?.kind
      || intent?.kind === "release" && value.request.number_id !== intent.numberId) throw new Error("invalid_request");
    const result = { ...project(value.request, ["kind", "status", "phone_number", "number_id", "created_at"]), operation_id: intent!.operationId, approval_request_id: operationId } as Record<string, unknown>;
    if (record(value.request.quote)) result.quote = project(value.request.quote, ["id", "currency", "monthly_price", "inbound_sms_price", "recurring", "expires_at"]);
    if (typeof value.request.error === "string" && PHONE_ERRORS.has(value.request.error)) result.error = value.request.error;
    return reply({ request: result }, response.status);
  } catch { return failure(request.method === "GET" ? 503 : 502, request.method === "GET" ? "phone_service_unavailable" : "phone_operation_outcome_unknown"); }
}
function project(value: unknown, fields: string[]): Record<string, unknown> {
  if (!record(value)) throw new Error("invalid_phone_response");
  return Object.fromEntries(fields.filter(key => ["string", "number", "boolean"].includes(typeof value[key])).map(key => [key, value[key]]));
}
async function phoneOperationId(grantId: string, operationId: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`connect-phone:${grantId}:${operationId}`))).slice(0, 16);
  bytes[6] = (bytes[6]! & 15) | 0x50; bytes[8] = (bytes[8]! & 63) | 0x80;
  const hex = [...bytes].map(b => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}
const PHONE_ERRORS = new Set(["phone_not_configured", "phone_provisioning_disabled", "phone_provisioning_limits_required", "invalid_request", "operation_conflict", "number_unavailable", "number_not_active", "number_limit_reached", "number_capacity_reached", "provider_unavailable", "pricing_unavailable", "rate_limited", "not_found", "price_changed", "provider_rejected", "provider_outcome_unknown"]);
