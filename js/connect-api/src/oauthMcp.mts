import type { Kv } from "accounts/server";
import { connectorCapabilities } from "./connectorPolicy.mts";

/** Public OAuth clients own exact redirect URIs. Metadata is never an assertion
 * that a client name, redirect host, or software publisher has been verified. */
export type McpClient = {
  client_id: string; client_name: string; redirect_uris: string[];
};
export type McpAuthorization = {
  client_id: string; client_name: string; redirect_uri: string; app_id: string; app_origin: string;
  resource: string; scope: string; resources: string[]; state?: string; challenge: string;
};
export type McpGrant = {
  id: string; token: string; appId: string; appOrigin: string; expiresAt: number;
  capabilities: readonly string[]; agentId: string; scope?: string;
  /** Internal refresh-family fence retained by durable event subscriptions. */
  familyId?: string;
};
type Family = { clientId: string; resource: string; scope: string; grant: McpGrant; revoked?: boolean };
type Token = { family: string };
type Code = { authorization: McpAuthorization; grant: McpGrant };
export type McpOAuthHooks = {
  requireDialog(request: Request): void;
  consentOrigin(request: Request): string;
  approve(request: Request, authorization: McpAuthorization, body: Record<string, unknown>): Promise<McpGrant>;
  active(grant: McpGrant): Promise<boolean>;
  revoke(grant: McpGrant): Promise<void>;
  registrationAllowed(request: Request): Promise<boolean>;
  authorizationAllowed(request: Request, clientId: string): Promise<boolean>;
};
export const mcpScopes = ["agent:run", "data:read", "data:write", "memory:read", "memory:write", "history:read", ...connectorCapabilities.filter(c => c !== "chatgpt").map(c => `connector:${c}`)] as const;
const opaque = /^[A-Za-z0-9_-]{43}$/;
const jsonHeaders = { "cache-control": "no-store", pragma: "no-cache", "referrer-policy": "no-referrer", "x-content-type-options": "nosniff" };

export class OAuthFailure extends Error {
  constructor(public status: number, public error: string, description: string) { super(description); }
}
function fail(error: string, description: string, status = 400): never { throw new OAuthFailure(status, error, description); }
export function oauthJson(body: unknown, status = 200, extra: Record<string, string> = {}): Response {
  return Response.json(body, { status, headers: { ...jsonHeaders, ...extra } });
}
function secret(): string { return btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32)))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, ""); }
async function hash(value: string): Promise<string> {
  return btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function record(value: unknown): value is Record<string, unknown> { return value !== null && typeof value === "object" && !Array.isArray(value); }
export async function boundedObject(request: Request, limit = 64 * 1024): Promise<Record<string, unknown>> {
  let value: unknown;
  try { value = JSON.parse(await boundedText(request, limit)); } catch (error) {
    if (error instanceof OAuthFailure) throw error;
    fail("invalid_json", "Malformed JSON.");
  }
  if (!record(value)) fail("invalid_request", "A JSON object is required.");
  return value;
}
async function boundedText(request: Request, limit: number): Promise<string> {
  const reader = request.body?.getReader();
  if (!reader) return "";
  let total = 0; const parts: Uint8Array[] = [];
  while (true) {
    const item = await reader.read(); if (item.done) break;
    total += item.value.byteLength;
    if (total > limit) { await reader.cancel(); fail("invalid_request", "Request body is too large.", 413); }
    parts.push(item.value);
  }
  const bytes = new Uint8Array(total); let offset = 0;
  for (const part of parts) { bytes.set(part, offset); offset += part.byteLength; }
  return new TextDecoder().decode(bytes);
}
function one(params: URLSearchParams, key: string, required = true): string {
  const values = params.getAll(key);
  if (values.length > 1 || (required && (values.length !== 1 || !values[0]))) fail("invalid_request", `Exactly one ${key} is required.`);
  return values[0] ?? "";
}
function redirectValid(value: unknown): value is string {
  if (typeof value !== "string" || value.length > 2048) return false;
  try {
    const url = new URL(value);
    return !url.username && !url.password && !url.hash && !value.includes("*") && url.href === value
      && !["code", "state", "iss", "error", "error_description"].some(key => url.searchParams.has(key))
      && (url.protocol === "https:" || (url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)));
  } catch { return false; }
}
function redirectMatches(registered: string, requested: string): boolean {
  if (registered === requested) return true;
  if (!redirectValid(requested)) return false;
  const a = new URL(registered), b = new URL(requested);
  if (a.protocol !== "http:" || !["localhost", "127.0.0.1", "[::1]"].includes(a.hostname)) return false;
  // RFC 8252 permits an ephemeral port, never a different host/path/query.
  a.port = b.port;
  return a.href === b.href;
}
function checkOrigin(request: Request, origins: readonly string[]): void {
  const origin = request.headers.get("origin");
  if (origin !== null && !origins.includes(origin)) fail("invalid_request", "This browser origin is not registered for this client.", 403);
}
function scopeValue(value: string): string {
  const scopes = value ? value.split(" ") : ["agent:run"];
  if (new Set(scopes).size !== scopes.length || scopes.some(scope => !mcpScopes.includes(scope as typeof mcpScopes[number]))) {
    fail("invalid_scope", "Request only advertised scopes.");
  }
  return scopes.join(" ");
}
function resourceSelection(scope: string, appId: string, origin: string, resource: string, requestId: string) {
  const base_resources = ["urn:nanocodex:agent:run", "urn:nanocodex:authorization:hosted",
    `urn:nanocodex:app:${encodeURIComponent(appId)}`, `urn:nanocodex:origin:${encodeURIComponent(origin)}`,
    `urn:nanocodex:mcp:resource:${encodeURIComponent(resource)}`, `urn:nanocodex:mcp:request:${requestId}`];
  const scope_resources = Object.fromEntries(scope.split(" ").map(item => [item, item === "agent:run"
    ? ["urn:nanocodex:agent:output:final", "urn:nanocodex:agent:output:actions", "urn:nanocodex:connector:chatgpt"]
    : [`urn:nanocodex:${item}`]]));
  return { base_resources, scope_resources };
}
function approvedResources(scope: string, appId: string, origin: string, resource: string, requestId: string): string[] {
  const { base_resources, scope_resources } = resourceSelection(scope, appId, origin, resource, requestId);
  return [...new Set([...base_resources, ...scope.split(" ").flatMap(item => scope_resources[item]!)])];
}
function callback(auth: Pick<McpAuthorization, "redirect_uri" | "resource" | "state">, values: Record<string, string>): string {
  const url = new URL(auth.redirect_uri);
  for (const [key, value] of Object.entries(values)) url.searchParams.set(key, value);
  url.searchParams.set("iss", new URL(auth.resource).origin);
  if (auth.state !== undefined) url.searchParams.set("state", auth.state);
  return url.href;
}
function ttl(family: Family): number { return Math.max(0, family.grant.expiresAt - Math.floor(Date.now() / 1000)); }
async function client(store: Kv.Kv, id: string): Promise<McpClient> {
  if (!opaque.test(id)) fail("invalid_client", "Unknown public client.", 401);
  const value = await store.get<McpClient>(`mcp-client:${id}`);
  if (!value) fail("invalid_client", "Unknown public client.", 401);
  return value;
}
async function liveFamily(store: Kv.Kv, id: string, hooks: McpOAuthHooks): Promise<Family | undefined> {
  const family = await store.get<Family>(`mcp-family:${id}`);
  if (!family || family.revoked || !ttl(family) || !await hooks.active(family.grant)) return undefined;
  return family;
}
async function revokeFamily(store: Kv.Kv, id: string, hooks: McpOAuthHooks): Promise<void> {
  const family = await store.get<Family>(`mcp-family:${id}`);
  if (!family) return;
  // Fence all access/refresh tokens before the downstream grant revocation.
  await store.set(`mcp-family:${id}`, { ...family, revoked: true }, { ttl: Math.max(60, ttl(family)) });
  await hooks.revoke(family.grant);
}
async function issue(store: Kv.Kv, id: string, family: Family): Promise<Response> {
  const access = secret(), refresh = secret();
  const remaining = ttl(family), accessTtl = Math.min(3600, remaining);
  if (!remaining) fail("invalid_grant", "The approved grant has expired.");
  await Promise.all([
    store.set(`mcp-access:${await hash(access)}`, { family: id } satisfies Token, { ttl: accessTtl }),
    store.set(`mcp-refresh:${await hash(refresh)}`, { family: id } satisfies Token, { ttl: remaining }),
    store.set(`mcp-used-refresh:${await hash(refresh)}`, { family: id } satisfies Token, { ttl: remaining }),
  ]);
  return oauthJson({ access_token: access, token_type: "Bearer", expires_in: accessTtl, refresh_token: refresh, scope: family.scope });
}
export async function authenticateMcp(request: Request, store: Kv.Kv, hooks: McpOAuthHooks): Promise<McpGrant | undefined> {
  const token = request.headers.get("authorization")?.match(/^Bearer ([A-Za-z0-9_-]{43})$/i)?.[1];
  if (!token) return undefined;
  const entry = await store.get<Token>(`mcp-access:${await hash(token)}`);
  const family = entry && await liveFamily(store, entry.family, hooks);
  if (!family || family.resource !== new URL("/mcp", request.url).href) return undefined;
  checkOrigin(request, [family.grant.appOrigin]);
  return { ...family.grant, scope: family.scope, familyId: entry!.family };
}
/** A subscription outlives an access token, but never its OAuth family or grant. */
export async function activeMcpGrant(reference: McpGrant, store: Kv.Kv, hooks: McpOAuthHooks): Promise<boolean> {
  if (!reference.familyId) return false;
  const family = await liveFamily(store, reference.familyId, hooks);
  return !!family && family.grant.id === reference.id && family.grant.token === reference.token
    && family.grant.appId === reference.appId && family.grant.appOrigin === reference.appOrigin
    && family.grant.agentId === reference.agentId && family.scope === reference.scope;
}
export async function oauthMcp(request: Request, store: Kv.Kv, hooks: McpOAuthHooks): Promise<Response | undefined> {
  const url = new URL(request.url), origin = url.origin, path = url.pathname;
  const resource = `${origin}/mcp`;
  let authorizationRedirect: Pick<McpAuthorization, "redirect_uri" | "resource" | "state"> | undefined;
  if (!["/.well-known/oauth-protected-resource", "/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-authorization-server"].includes(path)
    && !path.startsWith("/oauth/")) return undefined;
  try {
    if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: {
      ...jsonHeaders, "access-control-allow-origin": "*", "access-control-allow-methods": "GET, POST, OPTIONS", "access-control-allow-headers": "content-type, authorization",
    } });
    if (path.startsWith("/.well-known/")) {
      if (request.method !== "GET") fail("invalid_request", "Metadata requires GET.", 405);
      const body = path === "/.well-known/oauth-authorization-server" ? {
        issuer: origin, authorization_endpoint: `${origin}/oauth/authorize`, token_endpoint: `${origin}/oauth/token`,
        registration_endpoint: `${origin}/oauth/register`, revocation_endpoint: `${origin}/oauth/revoke`,
        authorization_response_iss_parameter_supported: true, response_types_supported: ["code"], grant_types_supported: ["authorization_code", "refresh_token"],
        token_endpoint_auth_methods_supported: ["none"], revocation_endpoint_auth_methods_supported: ["none"],
        code_challenge_methods_supported: ["S256"], scopes_supported: mcpScopes,
      } : { resource, authorization_servers: [origin], bearer_methods_supported: ["header"], scopes_supported: mcpScopes, resource_name: "Nanocodex" };
      return oauthJson(body, 200, { "access-control-allow-origin": "*" });
    }
    if (path === "/oauth/register") {
      if (request.method !== "POST") fail("invalid_request", "Registration requires POST.", 405);
      if (!request.headers.get("content-type")?.startsWith("application/json")) fail("invalid_request", "Registration requires application/json.", 415);
      if (!await hooks.registrationAllowed(request)) fail("temporarily_unavailable", "Registration rate limit exceeded.", 429);
      const body = await boundedObject(request, 16 * 1024);
      if (!Array.isArray(body.redirect_uris) || body.redirect_uris.length < 1 || body.redirect_uris.length > 8
        || !body.redirect_uris.every(redirectValid) || new Set(body.redirect_uris).size !== body.redirect_uris.length) fail("invalid_redirect_uri", "Register 1–8 exact HTTPS or loopback HTTP redirect URIs without fragments or credentials.");
      if ((body.token_endpoint_auth_method !== undefined && body.token_endpoint_auth_method !== "none")
        || (body.grant_types !== undefined && (!Array.isArray(body.grant_types) || body.grant_types.some(t => !["authorization_code", "refresh_token"].includes(String(t)))))
        || (body.response_types !== undefined && JSON.stringify(body.response_types) !== '["code"]')) fail("invalid_client_metadata", "Only public authorization-code clients with PKCE are supported.");
      if (body.client_name !== undefined && (typeof body.client_name !== "string" || !body.client_name.trim() || body.client_name.length > 128 || /[\x00-\x1f\x7f]/.test(body.client_name))) fail("invalid_client_metadata", "client_name must contain 1–128 printable characters.");
      const value: McpClient = { client_id: secret(), client_name: typeof body.client_name === "string" ? body.client_name.trim() : "MCP client", redirect_uris: body.redirect_uris };
      // DCR is optional compatibility support; it grants no account authority.
      await store.set(`mcp-client:${value.client_id}`, value, { ttl: 90 * 24 * 3600 });
      return oauthJson({ ...value, client_id_issued_at: Math.floor(Date.now() / 1000), token_endpoint_auth_method: "none", grant_types: ["authorization_code", "refresh_token"], response_types: ["code"] }, 201, { "access-control-allow-origin": "*" });
    }
    if (path === "/oauth/authorize") {
      if (request.method !== "GET") fail("invalid_request", "Authorization requires GET.", 405);
      const params = url.searchParams;
      const registered = await client(store, one(params, "client_id"));
      if (!await hooks.authorizationAllowed(request, registered.client_id)) fail("temporarily_unavailable", "Authorization rate limit exceeded.", 429);
      const redirect = one(params, "redirect_uri");
      if (!registered.redirect_uris.some(uri => redirectMatches(uri, redirect))) fail("invalid_request", "redirect_uri must exactly match a registered URI.");
      authorizationRedirect = { redirect_uri: redirect, resource };
      const state = one(params, "state", false);
      if (state.length > 2048) fail("invalid_request", "state is too long.");
      if (params.has("state")) authorizationRedirect.state = state;
      if (one(params, "response_type") !== "code") fail("unsupported_response_type", "Only code responses are supported.");
      if (one(params, "resource") !== resource) fail("invalid_target", "resource must exactly match this server's MCP URL.");
      if (one(params, "code_challenge_method") !== "S256" || !opaque.test(one(params, "code_challenge"))) fail("invalid_request", "A valid S256 PKCE challenge is required.");
      const scope = scopeValue(one(params, "scope", false));
      const appId = `mcp:${registered.client_id}`, appOrigin = new URL(redirect).origin;
      const id = secret();
      const authorization: McpAuthorization = { client_id: registered.client_id, client_name: registered.client_name,
        redirect_uri: redirect, app_id: appId, app_origin: appOrigin, resource, scope,
        resources: approvedResources(scope, appId, appOrigin, resource, id), challenge: one(params, "code_challenge"),
        ...(params.has("state") ? { state } : {}) };
      await store.set(`mcp-request:${id}`, authorization, { ttl: 600 });
      const consent = new URL("/connect-dialog", hooks.consentOrigin(request));
      consent.searchParams.set("oauth_request", id);
      // Explicit API origin preserves the issuer for canonical account routing.
      consent.searchParams.set("oauth_issuer", origin);
      return new Response(null, { status: 302, headers: { ...jsonHeaders, location: consent.href } });
    }
    const consent = path.match(/^\/oauth\/requests\/([A-Za-z0-9_-]{43})(?:\/(approve|deny))?$/);
    if (consent) {
      hooks.requireDialog(request);
      const key = `mcp-request:${consent[1]}`, authorization = await store.get<McpAuthorization>(key);
      if (!authorization || authorization.resource !== resource) fail("invalid_request", "This authorization request expired or has already been completed.");
      if (!consent[2] && request.method === "GET") {
        const { challenge: _, state: __, ...details } = authorization;
        return oauthJson({ ...details, ...resourceSelection(authorization.scope, authorization.app_id, authorization.app_origin, authorization.resource, consent[1]!) });
      }
      if (request.method !== "POST" || !consent[2]) fail("invalid_request", "Consent decisions require POST.", 405);
      if (consent[2] === "deny") {
        if (!store.take || !await store.take(key)) fail("invalid_request", "This authorization request was already completed.");
        return oauthJson({ redirect_uri: callback(authorization, { error: "access_denied", error_description: "The user declined access." }) });
      }
      const body = await boundedObject(request, 16 * 1024);
      if (body.scope !== undefined && (typeof body.scope !== "string" || !body.scope)) fail("invalid_scope", "Choose at least one requested scope.");
      const selected = body.scope === undefined ? authorization.scope : scopeValue(String(body.scope));
      if (selected.split(" ").some(scope => !authorization.scope.split(" ").includes(scope))) fail("invalid_scope", "Consent cannot add unrequested scopes.");
      const resources = approvedResources(selected, authorization.app_id, authorization.app_origin, authorization.resource, consent[1]!);
      if (!Array.isArray(body.resources) || body.resources.length !== resources.length || !resources.every(resource => (body.resources as unknown[]).includes(resource))) fail("invalid_scope", "Consent resources must exactly match the selected scopes and pending request.");
      const approved = { ...authorization, scope: selected, resources: body.resources as string[] };
      if (!store.create || !await store.create(`${key}:approving`, true, { ttl: 600 })) fail("invalid_request", "This authorization request is already being approved.");
      let grant: McpGrant;
      try { grant = await hooks.approve(request, approved, body); } catch (error) {
        await store.delete(`${key}:approving`);
        throw error;
      }
      if (!store.take || !await store.take(key)) { await hooks.revoke(grant); fail("invalid_request", "This authorization request was already completed."); }
      const code = secret();
      try { await store.set(`mcp-code:${await hash(code)}`, { authorization: approved, grant } satisfies Code, { ttl: 60 }); }
      catch (error) { await hooks.revoke(grant); throw error; }
      return oauthJson({ redirect_uri: callback(authorization, { code }) });
    }
    if (path === "/oauth/token" || path === "/oauth/revoke") {
      if (request.method !== "POST") fail("invalid_request", "This endpoint requires POST.", 405);
      if (!request.headers.get("content-type")?.startsWith("application/x-www-form-urlencoded")) fail("invalid_request", "Use application/x-www-form-urlencoded.", 415);
      if (request.headers.has("authorization")) fail("invalid_client", "This server supports public clients with token_endpoint_auth_method none.", 401);
      const params = new URLSearchParams(await boundedText(request, 16 * 1024));
      const registered = await client(store, one(params, "client_id"));

      if (params.has("client_secret")) fail("invalid_client", "Public clients must not send a client secret.", 401);
      if (path === "/oauth/revoke") {
        const token = one(params, "token");
        if (opaque.test(token)) {
          const digest = await hash(token);
          const entry = await store.get<Token>(`mcp-refresh:${digest}`) ?? await store.get<Token>(`mcp-access:${digest}`) ?? await store.get<Token>(`mcp-used-refresh:${digest}`);
          const family = entry && await store.get<Family>(`mcp-family:${entry.family}`);
          if (family) checkOrigin(request, [family.grant.appOrigin]);
          if (entry && family?.clientId === registered.client_id) await revokeFamily(store, entry.family, hooks);
        }
        return oauthJson({});
      }
      const type = one(params, "grant_type");
      if (one(params, "resource") !== resource) fail("invalid_target", "resource must exactly match this server's MCP URL.");
      if (type === "authorization_code") {
        const code = one(params, "code"), verifier = one(params, "code_verifier"), redirect = one(params, "redirect_uri");
        if (!opaque.test(code) || !/^[A-Za-z0-9._~-]{43,128}$/.test(verifier)) fail("invalid_grant", "Invalid authorization code or verifier.");
        const key = `mcp-code:${await hash(code)}`, entry = await store.get<Code>(key);
        if (!entry || entry.authorization.client_id !== registered.client_id || entry.authorization.redirect_uri !== redirect
          || entry.authorization.resource !== resource || await hash(verifier) !== entry.authorization.challenge || !await hooks.active(entry.grant)) fail("invalid_grant", "Authorization code, client, redirect, resource, or PKCE binding is invalid.");
        checkOrigin(request, [entry.grant.appOrigin]);
        if (params.has("scope") && one(params, "scope") !== entry.authorization.scope) fail("invalid_scope", "The approved scope cannot be changed.");
        if (!store.take || !await store.take(key)) fail("invalid_grant", "The authorization code was already used.");
        const id = secret(), family: Family = { clientId: registered.client_id, resource, scope: entry.authorization.scope, grant: entry.grant };
        await store.set(`mcp-family:${id}`, family, { ttl: ttl(family) });
        return issue(store, id, family);
      }
      if (type === "refresh_token") {
        const token = one(params, "refresh_token");
        if (!opaque.test(token)) fail("invalid_grant", "Invalid refresh token.");
        const digest = await hash(token), key = `mcp-refresh:${digest}`;
        const entry = await store.get<Token>(key) ?? await store.get<Token>(`mcp-used-refresh:${digest}`);
        const family = entry && await liveFamily(store, entry.family, hooks);
        if (!entry || !family || family.clientId !== registered.client_id || family.resource !== resource) fail("invalid_grant", "Refresh token is expired, revoked, or belongs to another client/resource.");
        checkOrigin(request, [family.grant.appOrigin]);
        if (params.has("scope") && one(params, "scope") !== family.scope) fail("invalid_scope", "The approved scope cannot be changed.");
        // Keep a replay tombstone for the full grant lifetime. Concurrent reuse
        // revokes the family even if another rotation is currently publishing.
        if (!store.take || !await store.take(key)) { await revokeFamily(store, entry.family, hooks); fail("invalid_grant", "Refresh token reuse revoked this authorization."); }
        await store.set(`mcp-used-refresh:${digest}`, entry, { ttl: ttl(family) });
        return issue(store, entry.family, family);
      }
      fail("unsupported_grant_type", "Use authorization_code or refresh_token.");
    }
    fail("invalid_request", "Unknown OAuth endpoint.", 404);
  } catch (error) {
    if (error instanceof OAuthFailure) {
      const values = { error: error.error === "invalid_json" ? "invalid_request" : error.error, error_description: error.message };
      // Once client and exact callback have been verified, OAuth authorization
      // failures return through that callback. Never redirect an invalid client
      // or URI, and never forward unvalidated caller-controlled destinations.
      if (authorizationRedirect) return new Response(null, { status: 302, headers: {
        ...jsonHeaders, location: callback(authorizationRedirect, values),
      } });
      return oauthJson(values, error.status);
    }
    if (typeof error === "object" && error !== null && "status" in error && "code" in error && error instanceof Error) {
      return oauthJson({ error: "access_denied", error_description: error.message + " If a requested connector is unavailable, connect it in Nanocodex and start authorization again." }, Number(error.status));
    }
    throw error;
  }
}
