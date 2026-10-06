import { createHostedRequest } from "nanocodex/services";
import type { Principal } from "./account-auth";
import { accountVaultMetadata } from "./account-info";
import { vaultRequest } from "./vault-request";

const PREFIX = "/v1/services";
const MAX_BODY_BYTES = 96 * 1024;
const MAX_RESPONSE_BYTES = 256 * 1024;
const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;

function json(value: unknown, status = 200): Response {
  return Response.json(value, { status, headers: {
    "cache-control": "no-store", "x-content-type-options": "nosniff",
  } });
}

async function text(body: ReadableStream<Uint8Array> | null, limit: number): Promise<string> {
  if (!body) return "";
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  let result = "", size = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return result + decoder.decode();
      size += value.byteLength;
      if (size > limit) { await reader.cancel(); throw new Error("request_too_large"); }
      result += decoder.decode(value, { stream: true });
    }
  } finally { reader.releaseLock(); }
}

/** Account APIs share authentication with the app, but never create an agent or turn. */
export async function routeServicesRequest(
  request: Request,
  broker: Fetcher,
  principal: Principal | null | undefined,
): Promise<Response> {
  if (!principal) return json({ error: "unauthorized" }, 401);
  if ((principal.kind !== "api_key" && principal.kind !== "account_session") || principal.connectGrant) {
    return json({ error: "forbidden" }, 403);
  }
  const url = new URL(request.url);
  if (url.pathname !== PREFIX && !url.pathname.startsWith(PREFIX + "/")) return json({ error: "not_found" }, 404);
  // Cookies cannot authorize a cross-origin mutation. Native/API clients use a scoped key.
  if (principal.kind === "account_session" && request.method !== "GET"
    && request.headers.get("origin") !== url.origin) return json({ error: "forbidden_origin" }, 403);
  const path = url.pathname.slice(PREFIX.length);
  const read = request.method === "GET";
  const required = read ? ["data:read"] : path === "/vault/request" ? ["tools:use"] : ["data:write", "tools:use"];
  if (!required.every(capability => (principal.capabilities as readonly string[]).includes(capability))) {
    return json({ error: "forbidden" }, 403);
  }
  if (path === "/links" && read) {
    const options: Record<string, string> = {};
    const keys: Record<string, string> = { service: "service", action: "action", kind: "kind", operation_id: "operationId", app_origin: "appOrigin", state: "state" };
    for (const [key, value] of url.searchParams) {
      if (!Object.hasOwn(keys, key) || !value || Object.hasOwn(options, keys[key]!)) return json({ error: "invalid_request" }, 400);
      options[keys[key]!] = value;
    }
    if ((options.service === "phone" && (options.kind || options.action)) || (options.service !== "phone" && options.operationId)
      || (!options.appOrigin && options.state)) return json({ error: "invalid_request" }, 400);
    try {
      // The destination is fixed by the service; request headers cannot redirect private input.
      return json(createHostedRequest({ ...options, host: "https://nanocodex.gakonst.workers.dev" } as Parameters<typeof createHostedRequest>[0]));
    } catch { return json({ error: "invalid_request" }, 400); }
  }
  if (path === "" && read && !url.search) {
    return json({ services: [
      { id: "vault", path: PREFIX + "/vault", operations: ["list", "get", "request"], links_path: PREFIX + "/links", secret_export: false },
      { id: "connectors", path: "/v1/connectors", catalog_path: "/v1/connectors/catalog", links_path: "/v1/account/links", management: "account_only" },
      { id: "totp", path: PREFIX + "/vault", enrollment_path: "/vault?service=totp", secret_export: false },
      { id: "phone", path: PREFIX + "/phone", human_approval_required: true, sms_2fa_compatibility: "provider_and_destination_dependent" },
    ] });
  }
  if ((path === "/vault" || path.startsWith("/vault/")) && read && !url.search) {
    const id = path === "/vault" ? undefined : path.slice("/vault/".length);
    if (id !== undefined && !VAULT_ID.test(id)) return json({ error: "not_found" }, 404);
    try {
      const vault = await accountVaultMetadata(broker, principal.userId, request.signal);
      if (id === undefined) return json({ vault });
      const entry = vault.find(item => item.id === id);
      return entry ? json({ entry }) : json({ error: "not_found" }, 404);
    } catch { return json({ error: "vault_unavailable" }, 503); }
  }
  if (path === "/vault/request" && request.method === "POST" && !url.search) {
    if (!isJson(request)) return json({ error: "invalid_request" }, 400);
    let input: unknown;
    try { input = JSON.parse(await text(request.body, MAX_BODY_BYTES)); }
    catch { return json({ error: "invalid_request" }, 400); }
    return vaultRequest(broker, { owner: principal.userId }, input, request.signal);
  }
  if (path.startsWith("/phone/")) {
    if (!/^(?:GET|POST|DELETE)$/.test(request.method)) return json({ error: "method_not_allowed" }, 405);
    // The broker owns the exact route schema. Never expose its internal control/webhook paths.
    const tail = path.slice("/phone".length);
    if (!/^\/(?:numbers(?:\/[-A-Za-z0-9_]+(?:\/(?:messages|release))?)?|requests(?:\/[-A-Za-z0-9_]+(?:\/(?:approve|deny))?)?|quotes|status)$/.test(tail)) {
      return json({ error: "not_found" }, 404);
    }
    const approve = /\/(?:approve|deny)$/.test(tail);
    if (approve && principal.kind !== "account_session") return json({ error: "human_approval_required" }, 403);
    const headers = new Headers();
    if (approve) headers.set("x-nanocodex-phone-human-approval", "true");
    let body: string | undefined;
    if (!read) {
      if (!isJson(request)) return json({ error: "invalid_request" }, 400);
      try {
        body = await text(request.body, MAX_BODY_BYTES);
        const value = JSON.parse(body);
        if (!value || typeof value !== "object" || Array.isArray(value)) return json({ error: "invalid_request" }, 400);
      } catch { return json({ error: "invalid_request" }, 400); }
      headers.set("content-type", "application/json");
    }
    const target = `https://phone-service.internal/v1/users/${encodeURIComponent(principal.userId)}${tail}${url.search}`;
    try {
      const response = await broker.fetch(new Request(target, {
        method: request.method, headers, ...(body === undefined ? {} : { body }),
        redirect: "manual", signal: request.signal,
      }));
      if (response.status === 204) return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
      if (!response.headers.get("content-type")?.toLowerCase().startsWith("application/json")) throw new Error("invalid_response");
      const value = JSON.parse(await text(response.body, MAX_RESPONSE_BYTES));
      return json(value, response.status);
    } catch {
      // A failed response is never permission to dispatch a second mutation.
      return json({ error: read ? "phone_service_unavailable" : "phone_operation_outcome_unknown" }, read ? 503 : 502);
    }
  }
  return json({ error: "not_found" }, 404);
}

function isJson(request: Request): boolean {
  return request.headers.get("content-type")?.toLowerCase().split(";")[0].trim() === "application/json";
}
