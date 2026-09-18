import { isHandViewerUpgrade, readManagedAccess, handRequestFailure, handBrokerRequest } from "nanocodex/cloudflare/managed-access";

export type ManagedProxyEnv = {
  NANOCODEX_BACKEND?: Fetcher;
  NANOCODEX_ACCESS_SECRET?: string;
  NANOCODEX_HAND_BROKER?: DurableObjectNamespace;
};

const MANAGED_ROUTE = /^(?:\/auth(?:\/.*)?|\/webauthn\/.*|\/sandbox-preview\/[^/]+(?:\/.*)?|\/v1\/(?:auth(?:\/.*)?|me|account\/(?:admin|communication|tool-host|vm-host|hand-hosts(?:\/[0-9a-f-]{36})?|hands(?:\/(?:screens|host|view|renew|ice))?)|hand-hosts\/[0-9a-f-]{36}\/[0-9a-f-]{36}\/hands\/(?:host|ice|renew)|system\/vm-host|vm-host-attachments\/[A-Za-z0-9_-]{43}\/[0-9a-f-]{36}\/(?:tool-host|hands\/(?:host|ice|renew))|wallet(?:\/(?:balance|connect|revoke-access-key))?|egress|api-keys(?:\/.*)?|credentials(?:\/.*)?|connect(?:\/.*)?|connectors(?:\/.*)?|agents(?:\/.*)?|rooms(?:\/.*)?|history(?:\/.*)?|memory(?:\/.*)?|organization(?:\/.*)?))$/;

export function isManagedRoutePath(pathname: string): boolean {
  return MANAGED_ROUTE.test(pathname) || /^\/v1\/phone\/bridge\/(?:health|check|calls(?:\/[0-9a-f-]{36}(?:\/(?:hangup|steer))?)?|status\/[0-9a-f-]{36}|media\/[0-9a-f-]{36}\/|internal\/(?:state|setup))$/.test(pathname);
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
  if (!env.NANOCODEX_BACKEND) {
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
  try {
    const started = performance.now();
    const startedAt = Date.now();
    // Only a locally verified, credential-bound snapshot skips the managed hop.
    // Every other request retains the original authenticator and rejection protocol.
    const cached = env.NANOCODEX_HAND_BROKER && isHandViewerUpgrade(request)
      ? await readManagedAccess(request, env) : undefined;
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
    } else response = await env.NANOCODEX_BACKEND.fetch(request);
    if (/^\/v1\/account\/hands\/(?:screens|host|view|ice|renew)$/.test(url.pathname)) {
      console.info({ type: "hand.proxy", request_id: response.headers.get("x-nanocodex-request-id"),
        method: request.method, path: url.pathname, status: response.status, route: local ? "local_access" : "managed",
        backend_ms: performance.now() - started, started_at_ms: startedAt, finished_at_ms: Date.now(),
        request_colo: typeof request.cf?.colo === "string" ? request.cf.colo : undefined });
    }
    return response;
  } catch (error) {
    console.error({
      type: "managed.backend_failure",
      path: url.pathname,
      error_kind: error instanceof Error ? error.name : typeof error,
    });
    return json({ error: "managed_service_unavailable" }, { status: 503 });
  }
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
