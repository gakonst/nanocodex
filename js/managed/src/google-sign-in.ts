import { Kv } from "accounts/server";

const TTL = 300;
const ID = /^[A-Za-z0-9_-]{43}$/;
const VERIFIER = /^[A-Za-z0-9._~-]{43,128}$/;
const COOKIE = "nanocodex_google_";
const CALLBACK = "/v1/connectors/google/callback";
const STATE_PREFIX = "signin.";
const STATE = /^signin\.[A-Za-z0-9_-]{43}$/;
const JWKS_URL = "https://www.googleapis.com/oauth2/v3/certs";

type Configuration = { GOOGLE_SIGN_IN?: Fetcher };
type Attempt = {
  mode: "browser" | "native";
  challenge: string;
  clientId: string;
  origin: string;
  expiresAt: number;
  browserBinding: string;
  linkUserId?: string;
};
type Authorization = { attemptId: string; nonce: string; verifier: string };
type Outcome = { status: "ready"; sub: string; completionDigest?: string } | { status: "failed"; error: string };
type Dependencies = {
  store: Kv.Kv;
  persistentUserId: () => Promise<string | undefined>;
  complete: (userId: string) => Promise<Response>;
};

// The dot cannot appear in the connector broker's base64url-only state.
// This only selects the handler; the callback still verifies browser binding and stored state.
export function isGoogleSignInCallback(url: URL): boolean {
  return url.pathname === CALLBACK && (url.searchParams.get("state")?.startsWith(STATE_PREFIX) ?? false);
}

function json(value: unknown, status = 200, headers?: HeadersInit) {
  const result = new Headers(headers);
  result.set("cache-control", "no-store");
  result.set("x-content-type-options", "nosniff");
  result.set("referrer-policy", "no-referrer");
  return Response.json(value, { status, headers: result });
}
function random() { return encode(crypto.getRandomValues(new Uint8Array(32))); }
function encode(value: Uint8Array) {
  return btoa(String.fromCharCode(...value)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
async function hash(value: string) {
  return encode(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))));
}
function cookie(request: Request, name: string) {
  return request.headers.get("cookie")?.split(";").map((part) => part.trim()).find((part) => part.startsWith(`${name}=`))?.slice(name.length + 1);
}
function bindingCookie(id: string, value: string, url: URL, age = TTL) {
  return `${COOKIE}${id}=${value}; Path=/v1/; Max-Age=${age}; HttpOnly; SameSite=Lax${url.protocol === "https:" ? "; Secure" : ""}`;
}
async function body(request: Request): Promise<Record<string, unknown> | undefined> {
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) return;
  const reader = request.body?.getReader();
  if (!reader) return;
  let text = "";
  let bytes = 0;
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  try {
    while (true) {
      const part = await reader.read();
      if (part.done) break;
      bytes += part.value.byteLength;
      if (bytes > 2048) { await reader.cancel(); return; }
      text += decoder.decode(part.value, { stream: true });
    }
    const value = JSON.parse(text + decoder.decode());
    return value && typeof value === "object" && !Array.isArray(value) ? value : undefined;
  } catch { return; } finally { reader.releaseLock(); }
}

/** Login only. Google Workspace connector consent is a separate flow. */
export async function routeGoogleSignIn(request: Request, env: Configuration, url: URL, deps: Dependencies): Promise<Response> {
  const operation = isGoogleSignInCallback(url) ? "callback" : url.pathname.slice("/v1/auth/google/".length);
  if (operation === "callback" && !isGoogleSignInCallback(url)) return json({ error: "not_found" }, 404);
  if (!["start", "authorize", "callback", "status", "complete", "cancel"].includes(operation)) return json({ error: "not_found" }, 404);
  if (request.method !== (operation === "authorize" || operation === "callback" ? "GET" : "POST")) return json({ error: "method_not_allowed" }, 405);
  if (request.method === "POST" && request.headers.get("origin") !== url.origin) return json({ error: "forbidden_origin" }, 403);
  const { store } = deps;
  if (!store.create || !store.take) return json({ error: "google_sign_in_unavailable" }, 503);
  const now = Math.floor(Date.now() / 1000);
  if (operation === "start") {
    const input = await body(request);
    if (!input || (input.intent !== undefined && input.intent !== "sign_in" && input.intent !== "link") || Object.keys(input).some((key) => !["mode", "code_challenge", "intent"].includes(key)) || (input.mode !== "browser" && input.mode !== "native") || typeof input.code_challenge !== "string" || !ID.test(input.code_challenge)) return json({ error: "invalid_google_request" }, 400);
    const linkUserId = input.intent === "link" && input.mode === "browser" ? await deps.persistentUserId() : undefined;
    if (input.intent === "link" && !linkUserId) return json({ error: "google_link_requires_account" }, 401);
    // Bound unauthenticated storage/provider work. The digest avoids persisting raw IPs.
    const ip = await hash(request.headers.get("cf-connecting-ip") ?? "local");
    let reserved = false;
    for (let slot = 0; slot < 20; slot++) {
      if (await store.create(`rate:${ip}:${Math.floor(now / 300)}:${slot}`, true, { ttl: TTL * 2 })) { reserved = true; break; }
    }
    if (!reserved) return json({ error: "rate_limited", retry_after: TTL }, 429, { "retry-after": String(TTL) });
    let clientId: string;
    try {
      const client = await brokerJson(env, "/v1/client");
      if (typeof client.client_id !== "string" || !client.client_id.trim() || client.client_id.length > 512) throw new Error("invalid client");
      clientId = client.client_id;
    } catch { return json({ error: "google_sign_in_unavailable" }, 503); }
    const id = random();
    const binding = random();
    const attempt: Attempt = { mode: input.mode, challenge: input.code_challenge, clientId, origin: url.origin, expiresAt: now + TTL, browserBinding: await hash(binding), ...(linkUserId ? { linkUserId } : {}) };
    await store.set(`attempt:${id}`, attempt, { ttl: TTL });
    await store.set(`active:${id}`, true, { ttl: TTL });
    // Browser start binds the existing browser before it opens the popup. Native
    // sessions get their binding only after opening the returned URL in ASWebAuthenticationSession.
    if (attempt.mode === "native") await store.set(`native-binding:${id}`, binding, { ttl: TTL });
    const authorization = new URL("/v1/auth/google/authorize", url.origin);
    authorization.searchParams.set("attempt_id", id);
    return json({ attempt_id: id, authorization_url: authorization.toString(), expires_in: TTL }, 200,
      attempt.mode === "browser" ? { "set-cookie": bindingCookie(id, binding, url) } : undefined);
  }
  if (operation === "callback") return callback(request, env, url, deps, now);
  const input = operation === "authorize" ? undefined : await body(request);
  const id = operation === "authorize" ? url.searchParams.get("attempt_id") : input?.attempt_id;
  if (typeof id !== "string" || !ID.test(id)) return json({ error: "invalid_google_attempt" }, 400);
  const attempt = await store.get<Attempt>(`attempt:${id}`);
  if (!attempt || !attempt.clientId || attempt.origin !== url.origin || attempt.expiresAt <= now) return json({ error: "invalid_or_expired_google_attempt" }, 400);
  if (operation === "authorize") {
    if (!await store.get(`active:${id}`)) return json({ error: "invalid_or_expired_google_attempt" }, 400);
    const nativeBinding = attempt.mode === "native" ? await store.take<string>(`native-binding:${id}`) : undefined;
    const binding = nativeBinding ?? cookie(request, `${COOKIE}${id}`);
    if (!binding || await hash(binding) !== attempt.browserBinding) return json({ error: "invalid_google_browser" }, 400);
    if (!await store.create(`authorized:${id}`, true, { ttl: TTL })) return json({ error: "invalid_or_expired_google_attempt" }, 400);
    const state = STATE_PREFIX + random();
    const nonce = random();
    const verifier = random();
    await store.set(`state:${state}`, { attemptId: id, nonce, verifier } satisfies Authorization, { ttl: Math.max(1, attempt.expiresAt - now) });
    const provider = new URL("https://accounts.google.com/o/oauth2/v2/auth");
    provider.search = new URLSearchParams({ client_id: attempt.clientId, redirect_uri: new URL(CALLBACK, url.origin).toString(), response_type: "code", scope: "openid email", state, nonce, code_challenge: await hash(verifier), code_challenge_method: "S256", prompt: "select_account" }).toString();
    return new Response(null, { status: 302, headers: { location: provider.toString(), "set-cookie": bindingCookie(id, binding, url, Math.max(1, attempt.expiresAt - now)), "cache-control": "no-store", "referrer-policy": "no-referrer" } });
  }
  if (typeof input?.code_verifier !== "string" || !VERIFIER.test(input.code_verifier) || await hash(input.code_verifier) !== attempt.challenge) return json({ error: "invalid_google_attempt" }, 400);
  if (await store.get(`cancelled:${id}`)) {
    if (operation === "status") return json({ status: "cancelled" });
    if (operation === "cancel") return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
    return json({ error: "invalid_or_expired_google_attempt" }, 400);
  }
  if (!await store.get(`active:${id}`)) return json({ error: "invalid_or_expired_google_attempt" }, 400);
  if (operation === "cancel") {
    if (!await store.take(`active:${id}`)) return json({ error: "invalid_or_expired_google_attempt" }, 400);
    await store.set(`cancelled:${id}`, true, { ttl: TTL });
    return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
  }
  const outcome = await store.get<Outcome>(`outcome:${id}`);
  if (operation === "status") return json(outcome?.status === "ready" ? { status: "ready" } : outcome ?? { status: "pending" });
  if (!outcome) return json({ error: "google_authorization_pending" }, 409);
  if (outcome.status === "failed") return json({ error: outcome.error }, 400);
  if (attempt.mode === "native" && (typeof input?.completion_code !== "string" || !ID.test(input.completion_code)
    || !outcome.completionDigest || await hash(input.completion_code) !== outcome.completionDigest)) return json({ error: "invalid_google_completion" }, 400);
  if (attempt.linkUserId && await deps.persistentUserId() !== attempt.linkUserId) return json({ error: "google_link_requires_same_account" }, 403);
  if (!await store.take(`active:${id}`)) return json({ error: "invalid_or_expired_google_attempt" }, 400);
  // The issuer is fixed and verified below. Never use email for identity or linking.
  const identityKey = `identity:${await hash(outcome.sub)}`;
  let identity = await store.get<{ userId: string }>(identityKey);
  if (!identity) {
    const proposed = { userId: attempt.linkUserId ?? crypto.randomUUID() };
    await store.create(identityKey, proposed);
    identity = await store.get<{ userId: string }>(identityKey);
  }
  if (!identity) return json({ error: "google_identity_unavailable" }, 503);
  if (attempt.linkUserId && identity.userId !== attempt.linkUserId) return json({ error: "google_identity_already_linked" }, 409);
  try { return await deps.complete(identity.userId); }
  catch { return json({ error: "google_account_unavailable" }, 503); }
}

async function callback(request: Request, env: Configuration, url: URL, { store }: Dependencies, now: number) {
  const state = url.searchParams.get("state");
  if (!state || !STATE.test(state)) return json({ error: "invalid_google_state" }, 400);
  const pending = await store.get<Authorization>(`state:${state}`);
  if (!pending) return json({ error: "invalid_google_state" }, 400);
  const attempt = await store.get<Attempt>(`attempt:${pending.attemptId}`);
  const binding = cookie(request, `${COOKIE}${pending.attemptId}`);
  if (!attempt || !attempt.clientId || attempt.origin !== url.origin || attempt.expiresAt <= now || !binding || await hash(binding) !== attempt.browserBinding || !await store.get(`active:${pending.attemptId}`)) return json({ error: "invalid_google_state" }, 400);
  // Check browser binding before consuming state, so a stolen URL cannot burn it.
  const authorization = await store.take!<Authorization>(`state:${state}`);
  if (!authorization) return json({ error: "invalid_google_state" }, 400);
  let outcome: Outcome;
  let completionCode: string | undefined;
  if (url.searchParams.get("error")) outcome = { status: "failed", error: url.searchParams.get("error") === "access_denied" ? "google_access_denied" : "google_authorization_failed" };
  else {
    try {
      const code = url.searchParams.get("code");
      if (!code || code.length > 4096) throw new Error("invalid code");
      const tokens = await brokerJson(env, "/v1/token", {
        client_id: attempt.clientId, redirect_uri: new URL(CALLBACK, attempt.origin).toString(),
        code, code_verifier: authorization.verifier,
      });
      const sub = await verifyIdToken(tokens.id_token, attempt.clientId, authorization.nonce, Math.floor(Date.now() / 1000));
      completionCode = attempt.mode === "native" ? random() : undefined;
      outcome = { status: "ready", sub, ...(completionCode ? { completionDigest: await hash(completionCode) } : {}) };
    } catch { outcome = { status: "failed", error: "google_authorization_failed" }; }
  }
  await store.create!(`outcome:${authorization.attemptId}`, outcome, { ttl: Math.max(1, attempt.expiresAt - now) });
  const headers = new Headers({ "cache-control": "no-store", "referrer-policy": "no-referrer", "x-content-type-options": "nosniff", "set-cookie": bindingCookie(authorization.attemptId, "", url, 0) });
  if (attempt.mode === "native") {
    const redirect = new URL("nanocodex://auth/google");
    redirect.search = new URLSearchParams({ attempt_id: authorization.attemptId, status: outcome.status }).toString();
    if (completionCode) redirect.searchParams.set("completion_code", completionCode);
    headers.set("location", redirect.toString());
    return new Response(null, { status: 302, headers });
  }
  headers.set("content-type", "text/html; charset=utf-8");
  const scriptNonce = random();
  headers.set("content-security-policy", `default-src 'none'; script-src 'nonce-${scriptNonce}'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'`);
  return new Response(`<!doctype html><meta charset="utf-8"><title>Google sign-in</title><p>${outcome.status === "ready" ? "Sign-in confirmed. Return to Nanocodex." : "Google sign-in was not completed. Return to Nanocodex and try again."}</p><script nonce="${scriptNonce}">window.close()</script>`, { headers });
}

// Only the connector broker holds the existing Google OAuth client secret.
// Pin its public client ID at start so rotations cannot change an in-flight login's audience.
async function brokerJson(env: Configuration, path: "/v1/client" | "/v1/token", input?: Record<string, string>) {
  if (!env.GOOGLE_SIGN_IN) throw new Error("Google sign-in broker unavailable");
  return responseJson(await env.GOOGLE_SIGN_IN.fetch(`https://google-sign-in.internal${path}`, {
    method: input ? "POST" : "GET", redirect: "manual", signal: AbortSignal.timeout(10_000),
    ...(input ? { headers: { "content-type": "application/json" }, body: JSON.stringify(input) } : {}),
  }));
}
async function providerJson(url: string): Promise<Record<string, unknown>> {
  return responseJson(await fetch(url, { redirect: "manual", signal: AbortSignal.timeout(10_000) }));
}
async function responseJson(response: Response): Promise<Record<string, unknown>> {
  if (!response.ok) { await response.body?.cancel(); throw new Error("provider failed"); }
  const text = await response.text();
  if (text.length > 128 * 1024) throw new Error("provider response too large");
  const result = JSON.parse(text);
  if (!result || typeof result !== "object" || Array.isArray(result)) throw new Error("invalid provider response");
  return result;
}
function decode(value: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("invalid JWT encoding");
  return Uint8Array.from(atob(value.replaceAll("-", "+").replaceAll("_", "/").padEnd(Math.ceil(value.length / 4) * 4, "=")), (char) => char.charCodeAt(0));
}
async function verifyIdToken(value: unknown, clientId: string, nonce: string, now: number): Promise<string> {
  if (typeof value !== "string" || value.length > 32_768) throw new Error("invalid id token");
  const parts = value.split(".");
  if (parts.length !== 3) throw new Error("invalid JWT");
  const header = JSON.parse(new TextDecoder().decode(decode(parts[0]!)));
  const claims = JSON.parse(new TextDecoder().decode(decode(parts[1]!)));
  if (!header || header.alg !== "RS256" || typeof header.kid !== "string" || header.kid.length > 256 || header.crit !== undefined) throw new Error("invalid JWT header");
  if (!claims || (claims.iss !== "https://accounts.google.com" && claims.iss !== "accounts.google.com")
    || !(claims.aud === clientId || (Array.isArray(claims.aud) && claims.aud.length > 0 && claims.aud.every((aud: unknown) => typeof aud === "string") && claims.aud.includes(clientId) && claims.azp === clientId))
    || (claims.azp !== undefined && claims.azp !== clientId)
    || !Number.isSafeInteger(claims.exp) || claims.exp <= now
    || !Number.isSafeInteger(claims.iat) || claims.iat > now + 60
    || (claims.nbf !== undefined && (!Number.isSafeInteger(claims.nbf) || claims.nbf > now + 60))
    || claims.nonce !== nonce || typeof claims.sub !== "string" || !claims.sub || claims.sub.length > 255) throw new Error("invalid JWT claims");
  const jwks = await providerJson(JWKS_URL);
  if (!Array.isArray(jwks.keys) || jwks.keys.length > 100) throw new Error("invalid JWKS");
  const matches = jwks.keys.filter((key) => key && key.kid === header.kid && key.kty === "RSA" && (key.alg === undefined || key.alg === "RS256") && (key.use === undefined || key.use === "sig"));
  if (matches.length !== 1) throw new Error("unknown signing key");
  const key = await crypto.subtle.importKey("jwk", matches[0], { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
  if (!await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, decode(parts[2]!), new TextEncoder().encode(`${parts[0]}.${parts[1]}`))) throw new Error("invalid JWT signature");
  if (claims.exp <= Math.floor(Date.now() / 1000)) throw new Error("expired JWT");
  return claims.sub;
}
