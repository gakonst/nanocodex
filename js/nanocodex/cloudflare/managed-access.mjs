export const MANAGED_ACCESS_HEADER = "x-nanocodex-access";
export const MANAGED_ACCESS_TTL_MS = 120_000;
const encoder = new TextEncoder();
let cachedKey;

/** Finite agent operations and screen viewer admission. Publishers, renewal and
 * agent streams retain live authentication; a viewer renews live every 10 s. */
export function managedAccessRequest(request) {
  const path = new URL(request.url).pathname;
  if (path === "/v1/account/hands/view") {
    return request.method === "GET" && request.headers.get("upgrade")?.toLowerCase() === "websocket";
  }
  if (path === "/v1/account/hands/screens" || path === "/v1/account/hands/ice") {
    return request.method === (path.endsWith("/screens") ? "GET" : "POST")
      && !request.headers.has("upgrade");
  }
  return /^\/v1\/agents(?:\/|$)/.test(path)
    && !request.headers.has("upgrade")
    && !request.headers.get("accept")?.includes("text/event-stream")
    && !/\/(?:ws|events|tool-host|device-host|sideband)$/.test(path);
}

function key(secret) {
  if (secret.length < 32) throw new Error("managed_access_not_configured");
  if (cachedKey?.secret !== secret) cachedKey = { secret, promise: crypto.subtle.importKey(
    "raw", encoder.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"],
  ) };
  return cachedKey.promise;
}
function encode(bytes) {
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
function decode(value) {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("invalid_managed_access");
  return Uint8Array.from(atob(value.replaceAll("-", "+").replaceAll("_", "/")), c => c.charCodeAt(0));
}

/** Bind reuse to the original login/key/grant, including all Connect restrictions. */
async function credentialBinding(request, kind) {
  const url = new URL(request.url);
  const connect = kind === "connect_grant" && url.origin === "https://nanocodex.internal" && request.headers.has("x-nanocodex-connect-user");
  // Match the live authenticator's first-cookie and whitespace semantics.
  let cookie;
  for (const part of request.headers.get("cookie")?.split(";") ?? []) {
    const separator = part.indexOf("=");
    if (separator >= 0 && part.slice(0, separator).trim() === "nanocodex_account") {
      cookie = part.slice(separator + 1).trim();
      break;
    }
  }
  const authorization = request.headers.get("authorization");
  if (!(connect || (kind === "account_session" && cookie) || (kind === "api_key" && authorization))) throw new Error("managed_access_requires_session");
  // Include every authority input so adding a cookie/Connect restriction cannot
  // make a cached API-key principal override the live authenticator's precedence.
  const identity = JSON.stringify({ kind, authorization, cookie: cookie ?? null,
    connect: [...request.headers].filter(([name]) => name.startsWith("x-nanocodex-connect-")).sort() });
  return encode(new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(identity))));
}

export async function readManagedAccess(request, env, now = Date.now()) {
  try {
    if (!managedAccessRequest(request)) return;
    const token = request.headers.get(MANAGED_ACCESS_HEADER);
    if (!token || token.length > 16_384) return;
    const [prefix, payload, signature, extra] = token.split(".");
    if (prefix !== "ncx_access_v1" || !payload || !signature || extra !== undefined) return;
    if (!await crypto.subtle.verify("HMAC", await key(env.NANOCODEX_ACCESS_SECRET ?? ""), decode(signature), encoder.encode(`${prefix}.${payload}`))) return;
    const value = JSON.parse(new TextDecoder().decode(decode(payload)));
    if (value.version !== 1 || value.audience !== new URL(request.url).origin
      || !Number.isSafeInteger(value.issuedAt) || !Number.isSafeInteger(value.expiresAt)
      || value.issuedAt > now || value.expiresAt <= now || value.expiresAt <= value.issuedAt
      || value.expiresAt - value.issuedAt > MANAGED_ACCESS_TTL_MS
      || !value.principal || !["account_session", "api_key", "connect_grant"].includes(value.principal.kind)
      || value.binding !== await credentialBinding(request, value.principal.kind)) return;
    return value.principal;
  } catch { return; }
}

/** Shared issuance keeps the exact audience, credential binding and lifetime wire contract. */
export async function createManagedAccessClaims(request, principal, now = Date.now()) {
  return { version: 1, audience: new URL(request.url).origin, binding: await credentialBinding(request, principal.kind),
    issuedAt: now, expiresAt: now + MANAGED_ACCESS_TTL_MS, principal };
}
export async function signManagedAccessClaims(claims, env) {
  const payload = `ncx_access_v1.${encode(encoder.encode(JSON.stringify(claims)))}`;
  const signature = await crypto.subtle.sign("HMAC", await key(env.NANOCODEX_ACCESS_SECRET ?? ""), encoder.encode(payload));
  return `${payload}.${encode(new Uint8Array(signature))}`;
}

export function isHandViewerUpgrade(request) {
  return request.method === "GET" && new URL(request.url).pathname === "/v1/account/hands/view"
    && request.headers.get("upgrade")?.toLowerCase() === "websocket";
}

/** Shared account Hand policy, applied after live or signed-snapshot authentication. */
export function handRequestFailure(request, principal) {
  const url = new URL(request.url);
  if (principal.connectGrant || !principal.capabilities.includes("agents:read")
    || !principal.capabilities.includes("tools:use")
    || (url.pathname.endsWith("/host") && !principal.capabilities.includes("agents:write"))) return "forbidden";
  if (principal.kind !== "api_key" && (request.method !== "GET" || request.headers.has("upgrade"))
    && request.headers.get("origin") !== url.origin) return "forbidden_origin";
}

/** Only for an authenticated principal that passed handRequestFailure. */
export function handBrokerRequest(request, principal) {
  const url = new URL(request.url);
  const headers = new Headers(request.headers);
  headers.delete("x-nanocodex-remote-vm");
  headers.set("x-nanocodex-owner-id", principal.userId);
  headers.set("x-nanocodex-session-organization-id", principal.organizationId);
  headers.set("x-nanocodex-session-team-id", principal.teamId);
  headers.set("x-nanocodex-authorization-epoch", String(principal.authorizationEpoch));
  headers.set("x-nanocodex-capabilities", JSON.stringify(principal.capabilities));
  for (const name of ["x-nanocodex-connect-user", "x-nanocodex-connect-grant-id", "x-nanocodex-connect-capabilities",
    "x-nanocodex-connect-connectors", "x-nanocodex-connect-connector-connections", "x-nanocodex-connect-mcp-ids",
    "x-nanocodex-connect-app-tool-catalog-digest"]) headers.delete(name);
  return new Request(`https://account-tools.internal${url.pathname.slice("/v1/account".length)}${url.search}`,
    new Request(request, { headers }));
}
