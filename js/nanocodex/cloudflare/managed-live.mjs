import { parseAgentSettingsQuery, agentSettingsQuery } from "./agent-settings.mjs";
import { API_KEY, forwardPrincipalAssertions } from "./managed-auth.mjs";
import { ingressColo } from "./durable-placement.mjs";

/** Narrow profile only; cookies and Connect retain the full authenticator's precedence. */
export function nativeLiveRequest(request) {
  return request.method === "GET" && new URL(request.url).pathname === "/v1/agents/live"
    && !request.headers.has("cookie")
    && ![...request.headers.keys()].some(name => name.startsWith("x-nanocodex-connect-"))
    && request.headers.get("authorization")?.startsWith("Bearer ") === true
    && API_KEY.test(request.headers.get("authorization").slice("Bearer ".length));
}
function failure(error, status) {
  return Response.json({ error }, { status, headers: { "cache-control": "no-store" } });
}
/** Preserve managed admission's query-before-upgrade-before-auth ordering. */
export function liveAgentSettings(request) {
  let settings;
  try { settings = parseAgentSettingsQuery(new URL(request.url).searchParams); }
  catch { return failure("invalid_request", 400); }
  if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  return settings;
}
export function liveAgentFailure(request, principal) {
  if (!principal) return failure("unauthorized", 401);
  if (!["agents:read", "agents:write", "tools:use"].every(c => principal.capabilities.includes(c))) return failure("forbidden", 403);
  if (principal.connectGrant && !principal.connectGrant.connectors.includes("chatgpt")) return failure("connector_forbidden", 403);
  if (principal.kind !== "api_key" && request.headers.get("origin") !== new URL(request.url).origin) return failure("forbidden_origin", 403);
}
/** Internal request only, after liveAgentFailure. Never accepts HTTP-supplied authority. */
export function liveAgentRequest(request, principal, settings, agentId, clientIngressColo) {
  const headers = new Headers(request.headers);
  forwardPrincipalAssertions(headers, principal);
  headers.delete("x-nanocodex-client-ingress-colo");
  headers.delete("x-nanocodex-worker-colo");
  const colo = ingressColo(clientIngressColo);
  if (colo) headers.set("x-nanocodex-client-ingress-colo", colo);
  headers.set("x-nanocodex-create-session-id", agentId);
  const query = agentSettingsQuery(settings);
  query.set("public_origin", new URL(request.url).origin);
  return new Request(`https://session.internal/create-live?${query}`, new Request(request, { headers }));
}
export function newManagedAgentId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let timestamp = BigInt(Date.now());
  for (let index = 5; index >= 0; index -= 1) {
    bytes[index] = Number(timestamp & 0xffn);
    timestamp >>= 8n;
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = [...bytes].map(byte => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
