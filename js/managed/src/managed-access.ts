import type { Principal } from "./account-auth";
import { MANAGED_ACCESS_HEADER, managedAccessRequest, createManagedAccessClaims, signManagedAccessClaims,
  readManagedAccess as readSharedManagedAccess, type ManagedAccessClaims, type ManagedAccessEnv } from "nanocodex/cloudflare/managed-access";
export { MANAGED_ACCESS_HEADER, MANAGED_ACCESS_TTL_MS, managedAccessRequest } from "nanocodex/cloudflare/managed-access";
export type { ManagedAccessEnv } from "nanocodex/cloudflare/managed-access";
type Claims = ManagedAccessClaims<Principal>;
const observations = new WeakMap<Request, { requestId: string; mode: "live" | "access"; authenticated: boolean;
  duration: number; startedAt: number; finishedAt: number; sessionMs?: number; claims?: Claims }>();

/** Agent upgrades retain live authority; this predicate enables only observations. */
function agentUpgradeRequest(request: Request): boolean {
  return request.method === "GET" && request.headers.get("upgrade")?.toLowerCase() === "websocket"
    && /^\/v1\/agents\/(?:live|[0-9a-f-]{36}\/(?:ws|tool-host|device-host))$/.test(new URL(request.url).pathname);
}

export function recordManagedSessionTiming(request: Request, duration: number): void {
  const observed = observations.get(request);
  if (observed) observed.sessionMs = duration;
}
export const readManagedAccess = (request: Request, env: ManagedAccessEnv, now = Date.now()) =>
  readSharedManagedAccess<Principal>(request, env, now);

export async function observeManagedAccess(request: Request, env: ManagedAccessEnv, principal: Principal | undefined,
  mode: "live" | "access", duration: number, startedAt = Date.now() - duration): Promise<void> {
  const reusable = managedAccessRequest(request);
  if (!reusable && !agentUpgradeRequest(request)) return;
  const finishedAt = Date.now();
  let claims: Claims | undefined;
  if (reusable && principal && mode === "live" && (env.NANOCODEX_ACCESS_SECRET?.length ?? 0) >= 32
    && principal.kind !== "service") {
    claims = await createManagedAccessClaims(request, principal);
  }
  observations.set(request, { requestId: crypto.randomUUID(), mode, authenticated: principal !== undefined,
    duration, startedAt, finishedAt, claims });
}

/** Piggyback issuance on live-authenticated responses: no extra cold-path round trip. */
export async function managedAccessResponse(request: Request, response: Response, env: ManagedAccessEnv): Promise<Response> {
  const observed = observations.get(request);
  if (!observed) return response;
  observations.delete(request);
  const headers = new Headers(response.headers);
  headers.set("x-nanocodex-request-id", observed.requestId);
  if (response.status === 401 && observed.mode === "access" && !observed.authenticated) {
    headers.set("x-nanocodex-access-rejected", "1");
  }
  headers.append("server-timing", `managed_auth;dur=${observed.duration.toFixed(1)};desc="${observed.mode}"`);
  if (observed.sessionMs !== undefined) {
    headers.append("server-timing", `managed_session;dur=${observed.sessionMs.toFixed(1)}`);
  }
  try {
    console.info({ type: "managed.auth", request_id: observed.requestId, mode: observed.mode, auth_ms: observed.duration,
      auth_started_at_ms: observed.startedAt, auth_finished_at_ms: observed.finishedAt,
      ...(observed.sessionMs === undefined ? {} : { session_ms: observed.sessionMs }),
      method: request.method, path: new URL(request.url).pathname, status: response.status, deployment_sha: env.DEPLOYMENT_SHA });
  } catch { /* Observations must not change admission or upgraded sockets. */ }
  if (observed.claims && response.ok && !/max-age|public/.test(headers.get("cache-control") ?? "")) {
    headers.set(MANAGED_ACCESS_HEADER, await signManagedAccessClaims(observed.claims, env));
    headers.set("x-nanocodex-access-ttl-ms", String(Math.max(0, observed.claims.expiresAt - Date.now())));
    headers.set("cache-control", "no-store");
  }
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers,
    ...(response.status === 101 ? { webSocket: response.webSocket } : {}),
  });
}
