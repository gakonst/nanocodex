import { consumeRpcData } from "nanocodex/cloudflare/rpc";
import { apiKeyDigest, apiKeyPrincipal } from "nanocodex/cloudflare/managed-auth";
import { nativeLiveRequest, liveAgentSettings, liveAgentFailure, liveAgentRequest, newManagedAgentId } from "nanocodex/cloudflare/managed-live";
import { durablePlacementOptions, ingressColo } from "nanocodex/cloudflare/durable-placement";

import { MANAGED_ACCESS_HEADER, MANAGED_ACCESS_TTL_MS, isHandViewerUpgrade, readManagedAccess, handRequestFailure, handBrokerRequest } from "nanocodex/cloudflare/managed-access";

export type ManagedProxyEnv = {
  NANOCODEX_BACKEND?: Fetcher;
  NANOCODEX_ACCESS_SECRET?: string;
  NANOCODEX_HAND_BROKER?: DurableObjectNamespace;
  NANOCODEX_LIVE_API_KEYS?: { getByName(name: string, options?: ReturnType<typeof durablePlacementOptions>): {
    resolveAuthorizedKey?: () => Promise<unknown>;
  } };
  NANOCODEX_LIVE_SESSIONS?: { getByName(name: string, options?: ReturnType<typeof durablePlacementOptions>): {
    fetch(request: Request): Promise<Response>;
  } };

};

const MANAGED_ROUTE = /^(?:\/auth(?:\/.*)?|\/webauthn\/.*|\/sandbox-preview\/[^/]+(?:\/.*)?|\/v1\/(?:auth(?:\/.*)?|me|account\/(?:admin|communication|hosted-tool-stats|tool-host|vm-host|hand-hosts(?:\/[0-9a-f-]{36})?|hands(?:\/(?:screens|host|view|renew|ice))?)|hand-hosts\/[0-9a-f-]{36}\/[0-9a-f-]{36}\/hands\/(?:host|ice|renew)|system\/vm-host|vm-host-attachments\/[A-Za-z0-9_-]{43}\/[0-9a-f-]{36}\/(?:tool-host|hands\/(?:host|ice|renew))|wallet(?:\/(?:balance|connect|revoke-access-key))?|egress|router|responses|models|inference(?:\/.*)?|api-keys(?:\/.*)?|credentials(?:\/.*)?|connect(?:\/.*)?|connectors(?:\/.*)?|agents(?:\/.*)?|meetings\/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\/preview|rooms(?:\/.*)?|history(?:\/.*)?|memories\/(?:list|read|search|add_ad_hoc_note|write|status)|markdown-memory\/(?:get|search|write|status)|organization(?:\/.*)?))$/;

export function isManagedRoutePath(pathname: string): boolean {
  return pathname === "/api/router" || pathname === "/v1/agent-runs" || MANAGED_ROUTE.test(pathname) || /^\/v1\/phone\/bridge\/(?:health|check|calls(?:\/[0-9a-f-]{36}(?:\/(?:hangup|steer))?)?|status\/[0-9a-f-]{36}|media\/[0-9a-f-]{36}\/|internal\/(?:state|setup))$/.test(pathname);
}

/**
 * Projects the private managed service onto the website origin.
 *
 * The managed service owns authentication, validation, account authorization,
 * room membership, and live WebSocket authorization. A verified short-lived
 * viewer snapshot can use the same shared policy and existing broker directly;
 * every other request preserves its exact original managed route.
 */
export async function routeManaged(
  request: Request,
  env: ManagedProxyEnv,
  url: URL,
): Promise<Response | undefined> {
  if (!isManagedRoutePath(url.pathname)) return undefined;
  if (/\bnci_/i.test(request.headers.get("authorization") ?? "")
    && url.pathname !== "/v1/responses" && url.pathname !== "/v1/models"
    && url.pathname !== "/v1/inference" && !url.pathname.startsWith("/v1/inference/")) {
    return json({ error: "inference_key_scope" }, { status: 403 });
  }
  if (!env.NANOCODEX_BACKEND) {
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
  try {
    const started = performance.now();
    const startedAt = Date.now();
    // Only a locally verified, credential-bound snapshot skips the managed hop.
    // Every other request retains the original authenticator and rejection protocol.
    const cached = env.NANOCODEX_HAND_BROKER && isHandViewerUpgrade(request)
      ? await viewerAccess(request, env) : undefined;
    const admitted = performance.now();
    const local = cached && !handRequestFailure(request, cached);
    let response: Response;
    if (local) {
      const brokerResponse = await env.NANOCODEX_HAND_BROKER!.getByName(cached.userId).fetch(handBrokerRequest(request, cached));
      const headers = new Headers(brokerResponse.headers);
      headers.set("x-nanocodex-request-id", crypto.randomUUID());
      headers.append("server-timing", `managed_auth;dur=${(admitted - started).toFixed(1)};desc="access", screen_route;dur=${(performance.now() - admitted).toFixed(1)}, screen_total;dur=${(performance.now() - started).toFixed(1)}`);
      response = new Response(brokerResponse.body, { status: brokerResponse.status, statusText: brokerResponse.statusText, headers,
        ...(brokerResponse.status === 101 ? { webSocket: brokerResponse.webSocket } : {}) });
    } else {
      if (url.pathname === "/api/router") {
        const target = new URL(request.url); target.pathname = "/v1/router";
        request = new Request(target, request);
      }
      // Undefined means ineligible/unconfigured before session creation. A failed
      // direct dispatch throws to the 503 boundary; never create a second agent.
      response = await directLiveAgent(request, env) ?? await env.NANOCODEX_BACKEND.fetch(request);
    }
    if (url.pathname === "/v1/agent-runs" || /^\/v1\/agents(?:\/(?:live|[0-9a-f-]{36}(?:\/(?:routing|settings|prepare|ws|events(?:\/history)?|turns(?:\/[A-Za-z0-9_.:-]{1,128}\/cancel)?))?))?$/.test(url.pathname)) {
      // Match the managed receipt without reading a body or changing upgraded
      // sockets. This separates account forwarding from managed execution and
      // the caller's network/scheduling residual in end-to-end traces.
      try {
        console.info({ type: "managed.proxy", request_id: response.headers.get("x-nanocodex-request-id"),
          method: request.method, path: url.pathname, status: response.status,
          backend_ms: performance.now() - started, started_at_ms: startedAt, finished_at_ms: Date.now(),
          request_colo: typeof request.cf?.colo === "string" ? request.cf.colo : undefined });
      } catch { /* Observation must preserve admission, streams and cancellation. */ }
    }
    if (/^\/v1\/account\/hands\/(?:screens|host|view|ice|renew)$/.test(url.pathname)) {
      console.info({ type: "hand.proxy", request_id: response.headers.get("x-nanocodex-request-id"),
        method: request.method, path: url.pathname, status: response.status, route: local ? "local_access" : "managed",
        backend_ms: performance.now() - started, started_at_ms: startedAt, finished_at_ms: Date.now(),
        request_colo: typeof request.cf?.colo === "string" ? request.cf.colo : undefined });
    }
    return await browserAccessResponse(request, response, env);
  } catch (error) {
    console.error({
      type: "managed.backend_failure",
      path: url.pathname,
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
}

/** API-key-only entrypoint; authority still comes from the existing live key DO. */
async function directLiveAgent(request: Request, env: ManagedProxyEnv): Promise<Response | undefined> {
  if (!env.NANOCODEX_LIVE_API_KEYS || !env.NANOCODEX_LIVE_SESSIONS || !nativeLiveRequest(request)) return;
  const settings = liveAgentSettings(request);
  if (settings instanceof Response) return settings;
  const started = performance.now();
  const startedAt = Date.now();
  const requestId = crypto.randomUUID();
  const digest = await apiKeyDigest(request);
  if (!digest) return;
  const colo = ingressColo(request.cf?.colo);
  const key = env.NANOCODEX_LIVE_API_KEYS.getByName(digest, durablePlacementOptions(colo));
  // Older/unconfigured bindings keep the full managed route, before any create.
  const resolve = key.resolveAuthorizedKey;
  if (typeof resolve !== "function") return;
  const principal = apiKeyPrincipal(consumeRpcData(await Reflect.apply(resolve, key, [])), digest);
  const admitted = performance.now();
  const authFinishedAt = Date.now();
  const failure = liveAgentFailure(request, principal);
  let response: Response;
  if (failure) response = failure;
  else {
    const agentId = newManagedAgentId();
    const internal = liveAgentRequest(request, principal!, settings, agentId, colo);
    let status: number | undefined;
    try {
      response = await env.NANOCODEX_LIVE_SESSIONS.getByName(agentId, durablePlacementOptions(colo)).fetch(internal);
      status = response.status;
    } finally {
      try {
        console.info({ type: "managed.agent.live_created", auth_kind: "api_key", route: "direct_live",
          request_id: requestId, agent_id: agentId, thread_id: agentId,
          outcome: status === 101 ? "success" : "failure", create_ms: Math.round((performance.now() - started) * 100) / 100,
          ...(status === undefined ? {} : { status }) });
      } catch { /* Correlation cannot alter a completed or ambiguous creation. */ }
    }
  }
  const headers = new Headers(response.headers);
  headers.set("x-nanocodex-request-id", requestId);
  headers.append("server-timing", `managed_auth;dur=${(admitted - started).toFixed(1)};desc="live", managed_session;dur=${(performance.now() - admitted).toFixed(1)}`);
  try {
    console.info({ type: "managed.auth", request_id: requestId, mode: "live", route: "direct_live",
      auth_ms: admitted - started, auth_started_at_ms: startedAt, auth_finished_at_ms: authFinishedAt,
      method: request.method, path: "/v1/agents/live", status: response.status });
  } catch { /* Observations cannot alter the upgrade. */ }
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers,
    ...(response.status === 101 ? { webSocket: response.webSocket } : {}) });
}

// A browser WebSocket cannot send the access header. Carry the existing signed
// snapshot in a host-only cookie; it never replaces the original session cookie.
const HAND_ACCESS_COOKIE = "__Secure-nanocodex_hand_access";
const HAND_ACCESS_COOKIE_SCOPE = "Path=/v1/account/hands; Secure; HttpOnly; SameSite=Strict";

function handAccessCookie(request: Request): string | undefined {
  const values = (request.headers.get("cookie") ?? "").split(";").flatMap(part => {
    const separator = part.indexOf("=");
    return separator >= 0 && part.slice(0, separator).trim() === HAND_ACCESS_COOKIE
      ? [part.slice(separator + 1).trim()] : [];
  });
  // Do not choose between conflicting path/domain cookies.
  return values.length === 1 && values[0] ? values[0] : undefined;
}

function browserOrigin(request: Request): boolean {
  const url = new URL(request.url);
  const origin = request.headers.get("origin");
  const site = request.headers.get("sec-fetch-site");
  return url.protocol === "https:" && (!origin || origin === url.origin)
    && (!site || site === "same-origin");
}

function accessVerificationRequest(request: Request, token: string): Request {
  const headers = new Headers(request.headers);
  headers.set(MANAGED_ACCESS_HEADER, token);
  // Verification uses only URL, method and headers; an ICE body may already
  // have been consumed by the managed service. Never clone or read that body.
  return new Request(request.url, { method: request.method, headers });
}

async function viewerAccess(request: Request, env: ManagedProxyEnv) {
  // An explicit header retains existing native/API admission and precedence.
  if (request.headers.has(MANAGED_ACCESS_HEADER)) return readManagedAccess(request, env);
  if (!browserOrigin(request) || request.headers.get("origin") !== new URL(request.url).origin) return;
  const token = handAccessCookie(request);
  if (!token) return;
  const principal = await readManagedAccess(accessVerificationRequest(request, token), env);
  return principal?.kind === "account_session" ? principal : undefined;
}

async function browserAccessResponse(request: Request, response: Response, env: ManagedProxyEnv): Promise<Response> {
  const path = new URL(request.url).pathname;
  if (!/^\/v1\/account\/hands(?:\/(?:screens|ice|view|renew))?$/.test(path) || !browserOrigin(request)) return response;
  let cookie: string | undefined;
  if ((response.status === 401 || response.status === 403) && handAccessCookie(request)) {
    cookie = `${HAND_ACCESS_COOKIE}=; Max-Age=0; ${HAND_ACCESS_COOKIE_SCOPE}`;
  } else if (response.ok && (path === "/v1/account/hands/screens" || path === "/v1/account/hands/ice")) {
    const token = response.headers.get(MANAGED_ACCESS_HEADER);
    const remaining = Number(response.headers.get("x-nanocodex-access-ttl-ms"));
    const maxAge = Math.floor(Math.min(remaining, MANAGED_ACCESS_TTL_MS) / 1_000);
    // Leave room under browser cookie limits; unsupported snapshots simply keep
    // the existing managed path. No issuance or re-signing happens here.
    if (token && token.length <= 3_800 && Number.isFinite(remaining) && maxAge > 0) {
      const principal = await readManagedAccess(accessVerificationRequest(request, token), env);
      if (principal?.kind === "account_session" && !handRequestFailure(request, principal)) {
        cookie = `${HAND_ACCESS_COOKIE}=${token}; Max-Age=${maxAge}; ${HAND_ACCESS_COOKIE_SCOPE}`;
      }
    }
  }
  if (!cookie) return response;
  const headers = new Headers(response.headers);
  headers.append("set-cookie", cookie);
  headers.set("cache-control", "no-store");
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

function json(body: unknown, init: ResponseInit): Response {
  return Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...init.headers,
    },
  });
}
