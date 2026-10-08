import { forwardManagedPreview, previewBridgeEnabled, type PreviewBridgeEnv } from "../../managed/src/preview-bridge.ts";
import { consumeRpcData } from "nanocodex/cloudflare/rpc";
import { apiKeyDigest, apiKeyPrincipal } from "nanocodex/cloudflare/managed-auth";
import { nativeLiveRequest, liveAgentSettings, liveAgentFailure, liveAgentRequest, newManagedAgentId, nativeRunRequest, nativeRunBody, runAgentRequest } from "nanocodex/cloudflare/managed-live";
import { durablePlacementOptions, ingressColo, placementRegion, regionalApiKeyAuthorityName, regionalApiKeyAuthorityRegion } from "nanocodex/cloudflare/durable-placement";

import { MANAGED_ACCESS_HEADER, MANAGED_ACCESS_TTL_MS, isHandViewerUpgrade, readManagedAccess, handRequestFailure, handBrokerRequest } from "nanocodex/cloudflare/managed-access";

export type ManagedProxyEnv = PreviewBridgeEnv & {
  NANOCODEX_BACKEND?: Fetcher;
  /** Private credential-only preparation; never sends a provider prompt. */
  NANOCODEX_SESSION_CREDENTIAL_PREWARM?: {
    prewarm(input: { owner: string; region: string }): Promise<unknown>;
  };
  NANOCODEX_ACCESS_SECRET?: string;
  NANOCODEX_HAND_BROKER?: DurableObjectNamespace;
  /** Regional screen relays (managed RegionalHandRelay); viewers of rs.<region>. generations admit there directly. */
  NANOCODEX_HAND_RELAYS?: { getByName(name: string, options?: { locationHint?: string }): { fetch(request: Request): Promise<Response> } };
  NANOCODEX_LIVE_API_KEYS?: {
    getByName(name: string, options?: ReturnType<typeof durablePlacementOptions>): {
      id?: { toString(): string };
      resolveAuthorizedKey?: () => Promise<unknown>;
      resolveRegionalAuthorizedKey?: (primaryObjectId: string, region: string) => Promise<unknown>;
    };
    idFromName?(name: string): { toString(): string };
  };
  /** "true" lets new ingress-regional lease replicas answer live API-key auth. */
  NANOCODEX_REGIONAL_API_KEY_AUTHORITY?: string;
  NANOCODEX_LIVE_SESSIONS?: { getByName(name: string, options?: ReturnType<typeof durablePlacementOptions>): {
    fetch(request: Request): Promise<Response>;
  } };

};

const PERMISSION_REQUEST_ROUTE = /^\/v1\/permission-requests(?:\/[A-Za-z0-9_-]{12}\/[0-9a-f-]{36}(?:\/(?:approve|deny))?)?$/;
const GENERATED_APP_ROUTE = /^\/v1\/apps(?:\/[A-Za-z0-9_-]{1,128}(?:\/(?:data|restore))?)?$/;
// This forwarding policy also runs in the Node route journeys. Native spans
// are available only inside Workers; their absence preserves the same policy.
const nativeTracing = import("nanocodex/cloudflare/tracing").catch(() => undefined);

const MANAGED_ROUTE = /^(?:\/auth(?:\/.*)?|\/webauthn\/.*|\/sandbox-preview\/[^/]+(?:\/.*)?|\/v1\/(?:auth(?:\/.*)?|me|admin\/threads|account\/(?:admin|communication|hosted-tool-stats|tool-host|vm-host|hand-hosts(?:\/[0-9a-f-]{36})?|hands(?:\/(?:screens|host|view|renew|ice))?)|hand-hosts\/[0-9a-f-]{36}\/[0-9a-f-]{36}\/hands\/(?:host|ice|renew)|system\/vm-host|vm-host-attachments\/[A-Za-z0-9_-]{43}\/[0-9a-f-]{36}\/(?:tool-host|hands\/(?:host|ice|renew))|wallet(?:\/(?:balance|connect|revoke-access-key|link(?:\/(?:poll|cancel))?|unlink))?|egress|data|crm(?:\/[A-Za-z0-9][A-Za-z0-9._:-]{0,127}(?:\/(?:identities|facts|relationships))?)?|router|responses|models|inference(?:\/.*)?|api-keys(?:\/.*)?|credentials(?:\/.*)?|connect(?:\/.*)?|connectors(?:\/.*)?|agents(?:\/.*)?|meetings(?:\/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}(?:\/(?:preview|summarize|audio(?:\/(?:complete|parts\/[1-9]\d*))?))?)?|todo(?:\/(?:snooze|traces|schedule|source-health|items\/[0-9a-fA-F-]{36}(?:\/prepare)?|decisions\/[0-9a-fA-F-]{36}(?:\/(?:respond|prepare))?|mail\/(?:accounts|threads(?:\/[A-Za-z0-9_-]+(?:\/modify)?)?|drafts(?:\/[0-9a-fA-F-]{36})?|send|suggest|messages\/[A-Za-z0-9_-]+\/attachments\/[A-Za-z0-9_-]+)))?|rooms(?:\/.*)?|history(?:\/.*)?|memories\/(?:list|read|search|add_ad_hoc_note|write|status)|markdown-memory\/(?:get|search|write|status)|organization(?:\/.*)?))$/;

// Hand IDs use the same portable identifier contract as publishers and SDKs.
// The only encoded punctuation in that contract is the colon in VM identities.
// Authentication and method-specific policy remain in the managed service.
const HAND_IDENTITY_ROUTE = /^\/v1\/account\/hands\/[A-Za-z0-9](?:[A-Za-z0-9._:-]|%3[Aa]){0,127}$/;

// Portable HLS: owner link management and token-authorized public playback/upload (tokens never in paths).
const SCREEN_PLAYBACK_ROUTE = /^\/v1\/(?:account\/hands\/playback-links(?:\/sp_[0-9a-f]{32})?|screen-playback\/sp_[0-9a-f]{32}\/(?:index\.m3u8|s(?:0|[1-9][0-9]{0,9})\.ts|upload\/(?:index\.m3u8|s(?:0|[1-9][0-9]{0,9})\.ts)?))$/;

export function isManagedRoutePath(pathname: string): boolean {
  return pathname === "/v1/teams" || pathname.startsWith("/v1/teams/") || SCREEN_PLAYBACK_ROUTE.test(pathname) || /^\/hand-share\/[0-9a-f-]{36}$/.test(pathname) || /^\/v1\/account\/hand-shares(?:\/(?:redeem|[0-9a-f-]{36}))?$/.test(pathname) || pathname === "/v1/services" || pathname.startsWith("/v1/services/") || pathname === "/v1/account/links" || pathname === "/v1/account/hands/inventory" || pathname === "/v1/account/hands/prune" || HAND_IDENTITY_ROUTE.test(pathname) || pathname === "/v1/account/hand-relays" || pathname === "/v1/account/hand-relays/retire" || /^\/v1\/vault\/(?:request|store|card)$/.test(pathname) || PERMISSION_REQUEST_ROUTE.test(pathname) || GENERATED_APP_ROUTE.test(pathname) || pathname === "/api/router" || pathname === "/v1/agent-runs" || MANAGED_ROUTE.test(pathname) || /^\/v1\/shared\/[0-9a-f-]{36}(?:\/(?:events(?:\/history)?|turns))?$/.test(pathname) || /^\/v1\/phone\/bridge\/(?:health|check|calls(?:\/[0-9a-f-]{36}(?:\/(?:hangup|steer))?)?|status\/[0-9a-f-]{36}|media\/[0-9a-f-]{36}\/|internal\/(?:state|setup))$/.test(pathname);
}

/**
 * Projects the private managed service onto the website origin.
 *
 * The managed service owns authentication, validation, account authorization,
 * room membership, and live WebSocket authorization. A verified short-lived
 * viewer snapshot can use the same shared policy and existing broker directly;
 * every other request preserves its exact original managed route.
 */
export async function routeManaged(...args: Parameters<typeof routeMeasuredManaged>): Promise<Response | undefined> {
  if (!isManagedRoutePath(args[2].pathname)) return undefined;
  const [request, , url] = args;
  // Connect only needs to discover an existing login. A cookie-free browser
  // cannot have one; avoid provisioning an anonymous account and wallet merely
  // to display the sign-in form. Existing cookies/authorization stay backend-owned.
  if (request.method === "GET" && url.pathname === "/v1/me" && url.searchParams.get("connect") === "1"
    && !request.headers.get("cookie") && !request.headers.get("authorization")) {
    return Response.json({ error: "unauthorized" }, { status: 401, headers: { "cache-control": "no-store", "vary": "Cookie, Authorization" } });
  }
  const native = await nativeTracing;
  if (!native) return routeMeasuredManaged(...args);
  const { tracing, setSpanAttributes } = native;
  return tracing.enterSpan("managed.proxy", span => {
    const threadId = args[2].pathname.match(/^\/v1\/agents\/([0-9a-f-]{36})(?:\/|$)/)?.[1];
    try { setSpanAttributes(span, { "nanocodex.thread_id": threadId }); }
    catch { /* Native tracing must not alter forwarding. */ }
    return routeMeasuredManaged(...args);
  });
}

async function routeMeasuredManaged(
  request: Request,
  env: ManagedProxyEnv,
  url: URL,
  context?: Pick<ExecutionContext, "waitUntil">,
): Promise<Response | undefined> {
  if (!isManagedRoutePath(url.pathname)) return undefined;
  if (/\bnci_/i.test(request.headers.get("authorization") ?? "")
    && url.pathname !== "/v1/responses" && url.pathname !== "/v1/models"
    && url.pathname !== "/v1/inference" && !url.pathname.startsWith("/v1/inference/")) {
    return json({ error: "inference_key_scope" }, { status: 403 });
  }
  // Preview routing must precede every production service and foreign-DO fast path.
  if (previewBridgeEnabled(env)) {
    if (url.pathname === "/api/router") {
      const target = new URL(request.url); target.pathname = "/v1/router";
      request = new Request(target, request);
    }
    return forwardManagedPreview(request, env);
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
      const brokered = handBrokerRequest(request, cached);
      // Regional generations name their relay; the owner DO is not on this path.
      const region = regionalScreenRegion(url.searchParams.get("generation"));
      const brokerResponse = region && env.NANOCODEX_HAND_RELAYS
        ? await env.NANOCODEX_HAND_RELAYS.getByName(`${cached.userId}:hand-relay:v1:${region}`, { locationHint: region })
          .fetch(regionalViewerRequest(brokered, cached.userId, region))
        : await env.NANOCODEX_HAND_BROKER!.getByName(cached.userId).fetch(brokered);
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
      response = await directLiveAgent(request, env, context) ?? await directAgentRun(request, env, context) ?? await env.NANOCODEX_BACKEND.fetch(request);
    }
    if (url.pathname === "/v1/agent-runs" || /^\/v1\/agents(?:\/(?:live|[0-9a-f-]{36}(?:\/(?:routing|settings|done|prepare|ws|events(?:\/history)?|turns(?:\/[A-Za-z0-9_.:-]{1,128}\/cancel)?))?))?$/.test(url.pathname)) {
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
        method: request.method, path: url.pathname, status: response.status,
        route: local ? (regionalScreenRegion(url.searchParams.get("generation")) && env.NANOCODEX_HAND_RELAYS ? "local_access_regional" : "local_access") : "managed",
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

// Mirrors managed regional-screen-routing without importing the managed DO graph:
// broker-minted "rs.<region>." generations name their relay; the owner is not on the path.
const HAND_RELAY_REGIONS = new Set(["wnam", "enam", "weur", "eeur", "apac", "oc", "sam", "afr", "me"]);
function regionalScreenRegion(id: string | null): string | undefined {
  const region = id ? /^rs\.([a-z]+)\./.exec(id)?.[1] : undefined;
  return region && HAND_RELAY_REGIONS.has(region) ? region : undefined;
}
function regionalViewerRequest(brokered: Request, owner: string, region: string): Request {
  const headers = new Headers(brokered.headers);
  headers.set("x-nanocodex-owner-id", owner);
  headers.set("x-nanocodex-hand-relay-region", region);
  return new Request(`https://account-tools.internal/hands/view${new URL(brokered.url).search}`, new Request(brokered, { headers }));
}

const INELIGIBLE = Symbol("ineligible");

/**
 * Live key authority. With regional authority enabled, ask the ingress
 * region's lease replica; it holds a lease only after the key's primary object
 * checked key, account and grant, and the primary revokes leases before
 * acknowledging a key deletion. A null answer or replica transport failure
 * falls back to the authoritative primary; a replica denial is final.
 */
async function liveKeyPrincipal(env: ManagedProxyEnv, digest: string, colo: string | null): Promise<ReturnType<typeof apiKeyPrincipal> | typeof INELIGIBLE> {
  const keys = env.NANOCODEX_LIVE_API_KEYS!;
  const region = regionalApiKeyAuthorityRegion(colo, env.NANOCODEX_REGIONAL_API_KEY_AUTHORITY);
  const primaryId = region && typeof keys.idFromName === "function" ? keys.idFromName(digest).toString() : undefined;
  if (region && primaryId && /^[0-9a-f]{64}$/.test(primaryId)) {
    const replica = keys.getByName(regionalApiKeyAuthorityName(primaryId, region), { locationHint: region });
    const resolve = replica.resolveRegionalAuthorizedKey;
    if (typeof resolve === "function") {
      let value: { record?: unknown; apiKeyObjectId?: unknown } | undefined | null | typeof INELIGIBLE;
      // Null means "ask the primary", exactly like a replica transport failure.
      try { value = consumeRpcData(await Reflect.apply(resolve, replica, [primaryId, region])) as typeof value | null; }
      catch { value = INELIGIBLE; }
      if (value === null) value = INELIGIBLE;
      if (value !== INELIGIBLE) {
        return value && value.apiKeyObjectId === primaryId ? apiKeyPrincipal(value.record, digest, primaryId) : undefined;
      }
    }
  }
  const key = keys.getByName(digest, durablePlacementOptions(colo));
  const resolve = key.resolveAuthorizedKey;
  if (typeof resolve !== "function") return INELIGIBLE;
  return apiKeyPrincipal(consumeRpcData(await Reflect.apply(resolve, key, [])), digest, key.id?.toString());
}

/** Authenticated optimization only. Failure never changes admission or retries it. */
function prewarmCredentials(env: ManagedProxyEnv, context: Pick<ExecutionContext, "waitUntil"> | undefined,
  owner: string, colo: string | null): void {
  const region = placementRegion(colo), binding = env.NANOCODEX_SESSION_CREDENTIAL_PREWARM;
  if (!region || !binding || !context) return;
  const started = performance.now();
  const observe = (value: unknown): void => {
    // Fixed outcome vocabulary only: no credential, owner or error contents.
    const outcome = value && typeof value === "object" && "outcome" in value
      && typeof value.outcome === "string"
      && ["warm", "filled", "unavailable", "invalid", "unsupported"].includes(value.outcome)
      ? value.outcome : "unavailable";
    try {
      console.info({ type: "managed.credential.prewarm", region, outcome,
        duration_ms: Math.round((performance.now() - started) * 100) / 100 });
    } catch { /* Observability must not change admission. */ }
  };
  try {
    context.waitUntil(binding.prewarm({ owner, region }).then(observe, () => observe(null)));
  } catch { observe(null); }
}

/** API-key-only entrypoint; authority still comes from the existing live key DO. */
async function directLiveAgent(request: Request, env: ManagedProxyEnv, context?: Pick<ExecutionContext, "waitUntil">): Promise<Response | undefined> {
  if (!env.NANOCODEX_LIVE_API_KEYS || !env.NANOCODEX_LIVE_SESSIONS || !nativeLiveRequest(request)) return;
  const settings = liveAgentSettings(request);
  if (settings instanceof Response) return settings;
  const started = performance.now();
  const startedAt = Date.now();
  const requestId = crypto.randomUUID();
  const digest = await apiKeyDigest(request);
  if (!digest) return;
  const colo = ingressColo(request.cf?.colo);
  const resolved = await liveKeyPrincipal(env, digest, colo);
  // Older/unconfigured bindings keep the full managed route, before any create.
  if (resolved === INELIGIBLE) return;
  const principal = resolved;
  const admitted = performance.now();
  const authFinishedAt = Date.now();
  const failure = liveAgentFailure(request, principal);
  let response: Response;
  if (failure) response = failure;
  else {
    prewarmCredentials(env, context, principal!.userId, colo);
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

/** Reuse the live key authority and Session boundary without an extra Worker hop. */
async function directAgentRun(request: Request, env: ManagedProxyEnv, context?: Pick<ExecutionContext, "waitUntil">): Promise<Response | undefined> {
  if (!env.NANOCODEX_LIVE_API_KEYS || !env.NANOCODEX_LIVE_SESSIONS || !nativeRunRequest(request)) return;
  const run = await nativeRunBody(request);
  if (!run) return;
  const startedAt = Date.now(), started = performance.now();
  const digest = await apiKeyDigest(request);
  if (!digest) return;
  const colo = ingressColo(request.cf?.colo);
  const resolved = await liveKeyPrincipal(env, digest, colo);
  if (resolved === INELIGIBLE) return;
  const principal = resolved;
  const authFinishedAt = Date.now();
  const failure = liveAgentFailure(request, principal);
  if (failure) return failure;
  prewarmCredentials(env, context, principal!.userId, colo);
  const internal = await runAgentRequest(request, principal!, run, colo);
  const dispatchAt = Date.now();
  const response = await env.NANOCODEX_LIVE_SESSIONS.getByName(internal.agentId, durablePlacementOptions(colo)).fetch(internal.request);
  if (response.ok && (!response.headers.get("content-type")?.startsWith("text/event-stream")
    || response.headers.get("x-nanocodex-agent-id") !== internal.agentId
    || response.headers.get("x-nanocodex-turn-id") !== internal.turnId)) {
    await response.body?.cancel();
    return json({ error: "turn_admission_invalid_response" }, { status: 502 });
  }
  const headers = new Headers(response.headers);
  let phases: Record<string, unknown> = {};
  try { phases = JSON.parse(headers.get("x-nanocodex-run-phases") ?? "{}"); } catch { /* Timing is optional. */ }
  headers.delete("x-nanocodex-run-phases");
  headers.set("x-nanocodex-request-id", crypto.randomUUID());
  try {
    console.info({ type: "managed.agent.run_created", route: "direct_run", thread_id: internal.agentId,
      turn_id: internal.turnId, status: response.status, auth_started_at_ms: startedAt,
      auth_finished_at_ms: authFinishedAt, session_dispatch_at_ms: dispatchAt, response_ready_at_ms: Date.now(),
      create_ms: performance.now() - started,
      session_constructor_entered_at_ms: phases.constructor_entered_at_ms,
      session_handler_entered_at_ms: phases.handler_entered_at_ms,
      session_handler_ms: phases.handler_ms, first_turn_admit_ms: phases.first_turn_admit_ms });
  } catch { /* Observations must not change admission or streaming. */ }
  return new Response(response.body, { status: response.status, headers });
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
