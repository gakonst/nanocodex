const PROVIDERS = new Set(["github", "gmail", "gdrive", "x", "slack"]);
const FLOWS = new Set(["connect", "managed"]);
const BASE64_URL = /^[A-Za-z0-9_-]+$/;
const INSTANCE_HOST = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/;
const SIGNING_CONTEXT = "nanocodex-local-oauth-relay-v1\0";
const HEALTH_CONTEXT = "nanocodex-local-oauth-relay-health-v1\0";
const STATE_TTL_SECONDS = 10 * 60;
const MAX_CLOCK_SKEW_SECONDS = 30;
const MAX_INNER_STATE_LENGTH = 512;
const MAX_ENVELOPE_LENGTH = 2_048;
const CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const CONNECTOR_CALLBACK = /^\/v1\/connectors\/(github|gmail|gdrive|x|slack)\/callback$/;
const MCP_CALLBACK = /^\/v1\/mcp-connections\/([A-Za-z0-9_-]{43})\/callback$/;
const SAFE_QUERY = ["code", "error", "error_description"];

export const LOCAL_OAUTH_RELAY_HOST = "127.0.0.1";
export const LOCAL_OAUTH_RELAY_PORT = 47_891;
export const LOCAL_OAUTH_RELAY_ORIGIN = `http://${LOCAL_OAUTH_RELAY_HOST}:${LOCAL_OAUTH_RELAY_PORT}`;

export function localOAuthRelayCallbackUrl(provider) {
  if (!PROVIDERS.has(provider)) return undefined;
  return `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/connectors/${provider}/callback`;
}

export function localMcpOAuthRelayCallbackUrl(connectionId) {
  if (typeof connectionId !== "string" || !CONNECTION_ID.test(connectionId)) return undefined;
  return `${LOCAL_OAUTH_RELAY_ORIGIN}/v1/mcp-connections/${connectionId}/callback`;
}

export function isLocalNanocodexOrigin(value) {
  let url;
  try { url = new URL(value); } catch { return false; }
  if (url.origin !== value || url.protocol !== "http:" || !url.port
    || url.username || url.password) return false;
  const port = Number(url.port);
  if (!Number.isSafeInteger(port) || port < 1_024 || port > 65_535
    || port === LOCAL_OAUTH_RELAY_PORT) return false;
  const hostname = url.hostname.toLowerCase();
  return hostname === "nanocodex.localhost" || INSTANCE_HOST.test(hostname);
}

export async function signLocalOAuthRelayState(
  { provider, connectionId, targetOrigin, flow, state },
  secret,
  { now = Date.now(), nonce } = {},
) {
  if (!validSecret(secret) || !validRelayTarget(provider, connectionId)
    || !isLocalNanocodexOrigin(targetOrigin) || !FLOWS.has(flow)
    || typeof state !== "string" || state.length < 1 || state.length > MAX_INNER_STATE_LENGTH) {
    throw new Error("invalid local OAuth relay state");
  }
  const issuedAt = Math.floor(now / 1_000);
  if (!Number.isSafeInteger(issuedAt) || issuedAt < 1) {
    throw new Error("invalid local OAuth relay time");
  }
  const envelope = {
    v: 1,
    ...(provider === undefined ? { c: connectionId } : { p: provider }),
    o: targetOrigin,
    f: flow,
    s: state,
    i: issuedAt,
    e: issuedAt + STATE_TTL_SECONDS,
    n: nonce ?? randomNonce(),
  };
  if (!validNonce(envelope.n)) throw new Error("invalid local OAuth relay nonce");
  const payload = encodeBase64Url(new TextEncoder().encode(JSON.stringify(envelope)));
  const signature = await sign(secret, payload);
  return `${payload}.${encodeBase64Url(signature)}`;
}

export async function verifyLocalOAuthRelayState(
  value,
  expectedProvider,
  secret,
  { now = Date.now() } = {},
) {
  if (!PROVIDERS.has(expectedProvider)) return undefined;
  return verifyRelayState(value, { provider: expectedProvider }, secret, { now });
}

export async function verifyLocalMcpOAuthRelayState(
  value,
  expectedConnectionId,
  secret,
  { now = Date.now() } = {},
) {
  if (typeof expectedConnectionId !== "string" || !CONNECTION_ID.test(expectedConnectionId)) {
    return undefined;
  }
  return verifyRelayState(value, { connectionId: expectedConnectionId }, secret, { now });
}

async function verifyRelayState(value, expected, secret, { now = Date.now() } = {}) {
  if (!validSecret(secret)
    || typeof value !== "string" || value.length > MAX_ENVELOPE_LENGTH) return undefined;
  const parts = value.split(".");
  if (parts.length !== 2) return undefined;
  const [payload, encodedSignature] = parts;
  if (!payload || !BASE64_URL.test(payload) || !encodedSignature || !BASE64_URL.test(encodedSignature)) {
    return undefined;
  }
  const signature = decodeBase64Url(encodedSignature);
  if (!signature || signature.byteLength !== 32) return undefined;
  const key = await hmacKey(secret, ["verify"]);
  const valid = await crypto.subtle.verify(
    "HMAC",
    key,
    signature,
    new TextEncoder().encode(`${SIGNING_CONTEXT}${payload}`),
  );
  if (!valid) return undefined;
  const decoded = decodeBase64Url(payload);
  if (!decoded) return undefined;
  let candidate;
  try { candidate = JSON.parse(new TextDecoder().decode(decoded)); } catch { return undefined; }
  const structurallyValid = expected.provider
    ? validEnvelope(candidate, expected.provider)
    : validMcpEnvelope(candidate, expected.connectionId);
  if (!structurallyValid) return undefined;
  const current = Math.floor(now / 1_000);
  if (!Number.isSafeInteger(current)
    || candidate.i > current + MAX_CLOCK_SKEW_SECONDS
    || current > candidate.e
    || candidate.e - candidate.i !== STATE_TTL_SECONDS) return undefined;
  return candidate;
}

export async function localOAuthRelayCallbackRedirect(url, secret, options) {
  if (!(url instanceof URL) || url.origin !== LOCAL_OAUTH_RELAY_ORIGIN || url.hash) return undefined;
  const provider = url.pathname.match(CONNECTOR_CALLBACK)?.[1];
  const connectionId = url.pathname.match(MCP_CALLBACK)?.[1];
  if (!provider && !connectionId) return undefined;
  const envelope = await verifyRelayState(url.searchParams.get("state"), {
    ...(provider ? { provider } : { connectionId }),
  }, secret, options);
  if (!envelope) return undefined;
  const destination = new URL(
    provider
      ? envelope.f === "connect"
        ? `/v1/connect/auth/connector-callback/${provider}`
        : `/v1/connectors/${provider}/callback`
      : envelope.f === "connect"
        ? `/v1/connect/auth/mcp-connection-callback/${connectionId}`
        : `/v1/mcp-connections/${connectionId}/callback`,
    envelope.o,
  );
  for (const name of SAFE_QUERY) {
    const value = url.searchParams.get(name);
    if (value !== null) destination.searchParams.set(name, value);
  }
  destination.searchParams.set("state", envelope.s);
  return destination;
}

export async function localOAuthRelayChallengeProof(challenge, secret) {
  if (!validSecret(secret) || typeof challenge !== "string"
    || challenge.length !== 43 || !BASE64_URL.test(challenge)) {
    throw new Error("invalid local OAuth relay challenge");
  }
  const key = await hmacKey(secret, ["sign"]);
  const signature = new Uint8Array(await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(`${HEALTH_CONTEXT}${challenge}`),
  ));
  return encodeBase64Url(signature);
}

function validEnvelope(value, provider) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const keys = Object.keys(value).sort().join(",");
  return keys === "e,f,i,n,o,p,s,v"
    && value.v === 1
    && value.p === provider
    && PROVIDERS.has(value.p)
    && isLocalNanocodexOrigin(value.o)
    && FLOWS.has(value.f)
    && typeof value.s === "string"
    && value.s.length > 0
    && value.s.length <= MAX_INNER_STATE_LENGTH
    && Number.isSafeInteger(value.i)
    && Number.isSafeInteger(value.e)
    && validNonce(value.n);
}

function validMcpEnvelope(value, connectionId) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const keys = Object.keys(value).sort().join(",");
  return keys === "c,e,f,i,n,o,s,v"
    && value.v === 1
    && value.c === connectionId
    && typeof value.c === "string"
    && CONNECTION_ID.test(value.c)
    && isLocalNanocodexOrigin(value.o)
    && FLOWS.has(value.f)
    && typeof value.s === "string"
    && value.s.length > 0
    && value.s.length <= MAX_INNER_STATE_LENGTH
    && Number.isSafeInteger(value.i)
    && Number.isSafeInteger(value.e)
    && validNonce(value.n);
}

function validRelayTarget(provider, connectionId) {
  return (PROVIDERS.has(provider) && connectionId === undefined)
    || (provider === undefined && typeof connectionId === "string" && CONNECTION_ID.test(connectionId));
}

function validSecret(secret) {
  return typeof secret === "string" && secret.length >= 32 && secret.length <= 1_024;
}

function validNonce(value) {
  return typeof value === "string" && value.length === 22 && BASE64_URL.test(value);
}

function randomNonce() {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return encodeBase64Url(bytes);
}

async function sign(secret, payload) {
  const key = await hmacKey(secret, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(`${SIGNING_CONTEXT}${payload}`),
  ));
}

function hmacKey(secret, usages) {
  return crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    usages,
  );
}

function encodeBase64Url(value) {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeBase64Url(value) {
  if (!value || !BASE64_URL.test(value)) return undefined;
  try {
    const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
    const padded = base64.padEnd(Math.ceil(base64.length / 4) * 4, "=");
    return Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
  } catch {
    return undefined;
  }
}
