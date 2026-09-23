import { MANAGED_ACCESS_HEADER, MANAGED_ACCESS_TTL_MS, isHandViewerUpgrade, readManagedAccess, handRequestFailure, handBrokerRequest } from "nanocodex/cloudflare/managed-access";

export type ManagedProxyEnv = {
  NANOCODEX_BACKEND?: Fetcher;
  NANOCODEX_ACCESS_SECRET?: string;
  NANOCODEX_HAND_BROKER?: DurableObjectNamespace;
};

const MANAGED_ROUTE = /^(?:\/auth(?:\/.*)?|\/webauthn\/.*|\/sandbox-preview\/[^/]+(?:\/.*)?|\/v1\/(?:auth(?:\/.*)?|me|account\/(?:admin|communication|tool-host|vm-host|hand-hosts(?:\/[0-9a-f-]{36})?|hands(?:\/(?:screens|host|view|renew|ice))?)|hand-hosts\/[0-9a-f-]{36}\/[0-9a-f-]{36}\/hands\/(?:host|ice|renew)|system\/vm-host|vm-host-attachments\/[A-Za-z0-9_-]{43}\/[0-9a-f-]{36}\/(?:tool-host|hands\/(?:host|ice|renew))|wallet(?:\/(?:balance|connect|revoke-access-key))?|egress|router|responses|models|inference(?:\/.*)?|api-keys(?:\/.*)?|credentials(?:\/.*)?|connect(?:\/.*)?|connectors(?:\/.*)?|agents(?:\/.*)?|rooms(?:\/.*)?|history(?:\/.*)?|memory(?:\/.*)?|organization(?:\/.*)?))$/;

export function isManagedRoutePath(pathname: string): boolean {
  return pathname === "/api/router" || MANAGED_ROUTE.test(pathname) || /^\/v1\/phone\/bridge\/(?:health|check|calls(?:\/[0-9a-f-]{36}(?:\/(?:hangup|steer))?)?|status\/[0-9a-f-]{36}|media\/[0-9a-f-]{36}\/|internal\/(?:state|setup))$/.test(pathname);
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
      response = await env.NANOCODEX_BACKEND.fetch(request);
    }
    if (/^\/v1\/agents(?:\/(?:live|[0-9a-f-]{36}(?:\/(?:routing|settings|prepare|ws|events(?:\/history)?|turns(?:\/[A-Za-z0-9_.:-]{1,128}\/cancel)?))?))?$/.test(url.pathname)) {
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
