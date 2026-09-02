import { Kv } from "accounts/server";

import { ensureAccount, isUserId, type AccountAuthEnv } from "./account-auth";

const INTERNAL_ORIGIN = "https://nanocodex.internal";
const EXCHANGE_TTL_DEFAULT = 120;
const EXCHANGE_TTL_MIN = 30;
const EXCHANGE_TTL_MAX = 300;
const MAX_BODY_BYTES = 20 * 1_024;
const MAX_RESOURCES = 64;
const MAX_RESOURCE_BYTES = 512;
const APP_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const OPAQUE = /^[A-Za-z0-9_-]{43}$/;
const BOUNDED_CLAIM = /^[^\u0000-\u001f\u007f]{1,512}$/;
const PROJECT_SECRET = /^\S{32,512}$/;

export interface HostPrincipalEnv extends AccountAuthEnv {
  NANOCODEX_HOST_PROJECTS?: string;
}

export type HostPrincipal = Readonly<{
  kind: "host";
  id: string;
  app_id: string;
  app_origin: string;
  issuer: string;
  tenant: string;
  session_epoch: number;
  session_digest: string;
}>;

type Project = Readonly<{
  appId: string;
  appOrigin: string;
  issuer: string;
  secretDigest: string;
  tenant: string;
}>;

type PrincipalMapping = Readonly<{
  appId: string;
  appOrigin: string;
  fence: string;
  issuer: string;
  principalId: string;
  sessionDigest: string;
  sessionEpoch: number;
  tenant: string;
  userId: string;
}>;

type HostSession = Readonly<{
  active: boolean;
  fence: string;
  issuer: string;
  principalId: string;
  sessionEpoch: number;
  tenant: string;
}>;

type Exchange = PrincipalMapping & Readonly<{
  expiresAt: number;
  resources: readonly string[];
}>;

export async function routeHostPrincipalRequest(
  request: Request,
  env: HostPrincipalEnv,
  url: URL,
): Promise<Response | undefined> {
  if (url.origin !== INTERNAL_ORIGIN) return undefined;
  if (url.pathname === "/connect/host-principals/exchanges") {
    return request.method === "POST"
      ? createExchange(request, env)
      : methodNotAllowed("POST");
  }
  if (url.pathname === "/connect/host-principals/sessions") {
    return request.method === "DELETE"
      ? revokeSession(request, env)
      : methodNotAllowed("DELETE");
  }
  if (url.pathname === "/connect/host-principals/exchange") {
    return request.method === "POST"
      ? consumeExchange(request, env)
      : methodNotAllowed("POST");
  }
  if (url.pathname === "/connect/host-principals/validate") {
    return request.method === "POST"
      ? validatePrincipal(request, env)
      : methodNotAllowed("POST");
  }
  return undefined;
}

async function createExchange(request: Request, env: HostPrincipalEnv): Promise<Response> {
  if (request.headers.has("origin")) return failure(403, "server_required");
  const parsed = await readObject(request);
  if (parsed instanceof Response) return parsed;
  if (!hasExactKeys(
    parsed,
    ["app_origin", "expires_in", "issuer", "resources", "session_id", "subject", "tenant"],
    ["expires_in"],
  )) {
    return failure(400, "invalid_exchange");
  }
  const appId = request.headers.get("x-nanocodex-app-id");
  const secret = request.headers.get("authorization")?.match(/^Bearer (\S{32,512})$/)?.[1];
  const appOrigin = publicOrigin(parsed.app_origin);
  const issuer = boundedClaim(parsed.issuer);
  const tenant = boundedClaim(parsed.tenant);
  const subject = boundedClaim(parsed.subject);
  const sessionId = boundedClaim(parsed.session_id);
  const resources = hostResources(parsed.resources);
  const expiresIn = parsed.expires_in ?? EXCHANGE_TTL_DEFAULT;
  if (!appId || !APP_ID.test(appId) || !secret || !PROJECT_SECRET.test(secret)
    || !appOrigin || !issuer || !tenant || !subject || !sessionId || !resources
    || !Number.isSafeInteger(expiresIn)
    || Number(expiresIn) < EXCHANGE_TTL_MIN
    || Number(expiresIn) > EXCHANGE_TTL_MAX) {
    return failure(400, "invalid_exchange");
  }
  const project = await authenticateProject(
    env.NANOCODEX_HOST_PROJECTS,
    appId,
    appOrigin,
    issuer,
    tenant,
    secret,
  );
  if (!project) return failure(401, "invalid_project");

  const principalId = await keyedDigest(
    secret,
    `host-principal:v2\0${appId}\0${appOrigin}\0${issuer}\0${tenant}\0${subject}`,
  );
  const sessionDigest = await keyedDigest(
    secret,
    `host-session:v2\0${appId}\0${appOrigin}\0${issuer}\0${tenant}\0${sessionId}`,
  );
  const store = hostStore(env);
  const locks = await acquireLocks(store, [`principal:${principalId}`, `session:${sessionDigest}`]);
  if (!locks) return failure(503, "exchange_unavailable");
  try {
    const mappingKey = `principal:${principalId}`;
    const sessionKey = `session:${sessionDigest}`;
    const currentMapping = await store.get<unknown>(mappingKey);
    const currentSession = await store.get<unknown>(sessionKey);
    const mapping = validMapping(currentMapping, principalId)
      ? currentMapping
      : {
          appId,
          appOrigin,
          fence: locks.token,
          issuer,
          principalId,
          sessionDigest,
          sessionEpoch: 1,
          tenant,
          userId: crypto.randomUUID(),
        } satisfies PrincipalMapping;
    if (currentMapping !== undefined && !validMapping(currentMapping, principalId)) {
      return failure(409, "principal_conflict");
    }
    if (currentSession !== undefined && !validSession(currentSession)) {
      return failure(409, "session_conflict");
    }
    if (validMapping(currentMapping, principalId)
      && (currentMapping.appId !== appId || currentMapping.appOrigin !== appOrigin
        || currentMapping.issuer !== issuer || currentMapping.tenant !== tenant)) {
      return failure(409, "principal_conflict");
    }
    if (validSession(currentSession) && !currentSession.active) {
      return failure(409, "session_revoked");
    }
    const sameActiveBinding = validMapping(currentMapping, principalId)
      && mapping.sessionDigest === sessionDigest
      && validSession(currentSession)
      && currentSession.active
      && currentSession.issuer === issuer
      && currentSession.principalId === principalId
      && currentSession.tenant === tenant
      && currentSession.sessionEpoch === mapping.sessionEpoch;
    const nextMapping = sameActiveBinding
      ? { ...mapping, fence: locks.token }
      : {
          ...mapping,
          fence: locks.token,
          sessionDigest,
          sessionEpoch: validMapping(currentMapping, principalId)
            ? Math.max(
                mapping.sessionEpoch,
                validSession(currentSession) ? currentSession.sessionEpoch : 0,
              ) + 1
            : validSession(currentSession) ? currentSession.sessionEpoch + 1 : 1,
        };
    const nextSession: HostSession = {
      active: true,
      fence: locks.token,
      issuer,
      principalId,
      sessionEpoch: nextMapping.sessionEpoch,
      tenant,
    };
    await Promise.all([
      store.set(mappingKey, nextMapping),
      store.set(sessionKey, nextSession),
      ensureAccount(env, nextMapping.userId, true),
    ]);
    const token = randomToken();
    const expiresAt = Math.floor(Date.now() / 1_000) + Number(expiresIn);
    const created = await store.create?.(`exchange:${await digest(token)}`, {
      ...nextMapping,
      expiresAt,
      resources,
    } satisfies Exchange, { ttl: Number(expiresIn) });
    return created
      ? json({ token, expires_at: expiresAt }, { status: 201 })
      : failure(503, "exchange_unavailable");
  } finally {
    await releaseLocks(store, locks);
  }
}

async function revokeSession(request: Request, env: HostPrincipalEnv): Promise<Response> {
  if (request.headers.has("origin")) return failure(403, "server_required");
  const parsed = await readObject(request);
  if (parsed instanceof Response) return parsed;
  if (!hasExactKeys(parsed, ["app_origin", "issuer", "session_id", "subject", "tenant"])) {
    return failure(400, "invalid_session");
  }
  const appId = request.headers.get("x-nanocodex-app-id");
  const secret = request.headers.get("authorization")?.match(/^Bearer (\S{32,512})$/)?.[1];
  const appOrigin = publicOrigin(parsed.app_origin);
  const issuer = boundedClaim(parsed.issuer);
  const tenant = boundedClaim(parsed.tenant);
  const subject = boundedClaim(parsed.subject);
  const sessionId = boundedClaim(parsed.session_id);
  if (!appId || !APP_ID.test(appId) || !secret || !PROJECT_SECRET.test(secret)
    || !appOrigin || !issuer || !tenant || !subject || !sessionId
    || !await authenticateProject(
      env.NANOCODEX_HOST_PROJECTS,
      appId,
      appOrigin,
      issuer,
      tenant,
      secret,
    )) {
    return failure(401, "invalid_project");
  }
  const principalId = await keyedDigest(
    secret,
    `host-principal:v2\0${appId}\0${appOrigin}\0${issuer}\0${tenant}\0${subject}`,
  );
  const sessionDigest = await keyedDigest(
    secret,
    `host-session:v2\0${appId}\0${appOrigin}\0${issuer}\0${tenant}\0${sessionId}`,
  );
  const store = hostStore(env);
  const locks = await acquireLocks(store, [`principal:${principalId}`, `session:${sessionDigest}`]);
  if (!locks) return failure(503, "session_unavailable");
  try {
    const mappingKey = `principal:${principalId}`;
    const sessionKey = `session:${sessionDigest}`;
    const mapping = await store.get<unknown>(mappingKey);
    const session = await store.get<unknown>(sessionKey);
    if (validMapping(mapping, principalId)
      && mapping.appId === appId
      && mapping.appOrigin === appOrigin
      && mapping.issuer === issuer
      && mapping.sessionDigest === sessionDigest
      && mapping.tenant === tenant) {
      const epoch = mapping.sessionEpoch + 1;
      await Promise.all([
        store.set(mappingKey, { ...mapping, fence: locks.token, sessionEpoch: epoch }),
        store.set(sessionKey, {
          active: false,
          fence: locks.token,
          issuer,
          principalId,
          sessionEpoch: epoch,
          tenant,
        } satisfies HostSession),
      ]);
    } else if (validSession(session) && session.principalId === principalId
      && session.issuer === issuer && session.tenant === tenant) {
      await store.set(sessionKey, {
        ...session,
        active: false,
        fence: locks.token,
        sessionEpoch: session.sessionEpoch + 1,
      });
    } else if (!session) {
      await store.set(sessionKey, {
        active: false,
        fence: locks.token,
        issuer,
        principalId,
        sessionEpoch: 1,
        tenant,
      } satisfies HostSession);
    }
    return new Response(null, { status: 204, headers: noStoreHeaders() });
  } finally {
    await releaseLocks(store, locks);
  }
}

async function consumeExchange(request: Request, env: HostPrincipalEnv): Promise<Response> {
  const parsed = await readObject(request);
  if (parsed instanceof Response) return parsed;
  if (!hasExactKeys(parsed, ["app_id", "app_origin", "exchange", "resources"])) {
    return failure(400, "invalid_exchange");
  }
  const appId = typeof parsed.app_id === "string" && APP_ID.test(parsed.app_id) ? parsed.app_id : undefined;
  const appOrigin = publicOrigin(parsed.app_origin);
  const token = typeof parsed.exchange === "string" && OPAQUE.test(parsed.exchange) ? parsed.exchange : undefined;
  const resources = hostResources(parsed.resources);
  if (!appId || !appOrigin || !token || !resources) return failure(400, "invalid_exchange");
  const store = hostStore(env);
  const exchange = await store.take?.<unknown>(`exchange:${await digest(token)}`);
  const now = Math.floor(Date.now() / 1_000);
  if (!validExchange(exchange) || exchange.expiresAt <= now
    || exchange.appId !== appId || exchange.appOrigin !== appOrigin
    || !equalResources(exchange.resources, resources)) return failure(403, "invalid_exchange");
  const active = await activeBinding(store, hostPrincipal(exchange), exchange.userId);
  if (!active) return failure(403, "host_session_fenced");
  return json({
    principal: hostPrincipal(exchange),
    user_id: exchange.userId,
    resources: exchange.resources,
    expires_at: exchange.expiresAt,
  });
}

async function validatePrincipal(request: Request, env: HostPrincipalEnv): Promise<Response> {
  const parsed = await readObject(request);
  if (parsed instanceof Response) return parsed;
  if (!hasExactKeys(parsed, ["principal"]) || !validHostPrincipal(parsed.principal)) {
    return failure(400, "invalid_principal");
  }
  const store = hostStore(env);
  const mapping = await store.get<unknown>(`principal:${parsed.principal.id}`);
  if (!validMapping(mapping, parsed.principal.id)
    || !await activeBinding(store, parsed.principal, mapping.userId)) {
    return failure(403, "host_session_fenced");
  }
  return json({ active: true, user_id: mapping.userId });
}

async function activeBinding(store: Kv.Kv, principal: HostPrincipal, userId: string): Promise<boolean> {
  const [mapping, session, principalFence, sessionFence] = await Promise.all([
    store.get<unknown>(`principal:${principal.id}`),
    store.get<unknown>(`session:${principal.session_digest}`),
    store.get<unknown>(`fence:principal:${principal.id}`),
    store.get<unknown>(`fence:session:${principal.session_digest}`),
  ]);
  return validMapping(mapping, principal.id)
    && mapping.userId === userId
    && mapping.appId === principal.app_id
    && mapping.appOrigin === principal.app_origin
    && mapping.issuer === principal.issuer
    && mapping.tenant === principal.tenant
    && mapping.sessionDigest === principal.session_digest
    && mapping.sessionEpoch === principal.session_epoch
    && mapping.fence === principalFence
    && validSession(session)
    && session.active
    && session.principalId === principal.id
    && session.issuer === principal.issuer
    && session.tenant === principal.tenant
    && session.sessionEpoch === principal.session_epoch
    && session.fence === sessionFence
    && principalFence === sessionFence;
}

function hostPrincipal(value: PrincipalMapping): HostPrincipal {
  return {
    kind: "host",
    id: value.principalId,
    app_id: value.appId,
    app_origin: value.appOrigin,
    issuer: value.issuer,
    tenant: value.tenant,
    session_epoch: value.sessionEpoch,
    session_digest: value.sessionDigest,
  };
}

function hostStore(env: HostPrincipalEnv): Kv.Kv {
  return Kv.durableObject(env.NANOCODEX_AUTH as unknown as Kv.durableObject.Namespace, {
    name: "host-principals",
  });
}

async function authenticateProject(
  encoded: string | undefined,
  appId: string,
  appOrigin: string,
  issuer: string,
  tenant: string,
  secret: string,
): Promise<Project | undefined> {
  let projects: readonly Project[];
  try { projects = parseProjects(encoded); } catch { return undefined; }
  const project = projects.find((value) => value.appId === appId
    && value.appOrigin === appOrigin
    && value.issuer === issuer
    && value.tenant === tenant);
  if (!project) return undefined;
  return constantTimeEqual(await digest(secret), project.secretDigest) ? project : undefined;
}

export function parseProjects(encoded: string | undefined): readonly Project[] {
  if (!encoded) return [];
  const value = JSON.parse(encoded) as unknown;
  if (!Array.isArray(value) || value.length > 128) throw new Error("invalid host projects");
  const seen = new Set<string>();
  return value.map((entry) => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)
      || !hasExactKeys(
        entry as Record<string, unknown>,
        ["app_id", "app_origin", "issuer", "secret_sha256", "tenant"],
      )) {
      throw new Error("invalid host project");
    }
    const record = entry as Record<string, unknown>;
    const appId = typeof record.app_id === "string" && APP_ID.test(record.app_id) ? record.app_id : undefined;
    const appOrigin = publicOrigin(record.app_origin);
    const issuer = boundedClaim(record.issuer);
    const secretDigest = typeof record.secret_sha256 === "string" && OPAQUE.test(record.secret_sha256)
      ? record.secret_sha256 : undefined;
    const tenant = boundedClaim(record.tenant);
    const projectKey = `${appId}\0${appOrigin}\0${issuer}\0${tenant}`;
    if (!appId || !appOrigin || !issuer || !secretDigest || !tenant || seen.has(projectKey)) {
      throw new Error("invalid host project");
    }
    seen.add(projectKey);
    return { appId, appOrigin, issuer, secretDigest, tenant };
  });
}

function hostResources(value: unknown): readonly string[] | undefined {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_RESOURCES) return undefined;
  const seen = new Set<string>();
  const result: string[] = [];
  for (const item of value) {
    if (typeof item !== "string" || !item
      || new TextEncoder().encode(item).byteLength > MAX_RESOURCE_BYTES
      || seen.has(item) || forbiddenAuthority(item)) return undefined;
    seen.add(item);
    result.push(item);
  }
  return result;
}

function forbiddenAuthority(resource: string): boolean {
  const value = resource.toLowerCase();
  return value.startsWith("urn:nanocodex:mpp:")
    || value.startsWith("urn:nanocodex:access-key:")
    || value.startsWith("urn:nanocodex:access_key:")
    || value === "urn:nanocodex:authorize-access-key";
}

function validHostPrincipal(value: unknown): value is HostPrincipal {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  return hasExactKeys(
    record,
    ["app_id", "app_origin", "id", "issuer", "kind", "session_digest", "session_epoch", "tenant"],
  )
    && record.kind === "host"
    && typeof record.id === "string" && OPAQUE.test(record.id)
    && typeof record.app_id === "string" && APP_ID.test(record.app_id)
    && typeof record.app_origin === "string"
    && publicOrigin(record.app_origin) === record.app_origin
    && boundedClaim(record.issuer) === record.issuer
    && boundedClaim(record.tenant) === record.tenant
    && Number.isSafeInteger(record.session_epoch) && Number(record.session_epoch) >= 1
    && typeof record.session_digest === "string" && OPAQUE.test(record.session_digest);
}

function validMapping(value: unknown, principalId: string): value is PrincipalMapping {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Partial<PrincipalMapping>;
  return hasExactKeys(
    value as Record<string, unknown>,
    ["appId", "appOrigin", "fence", "issuer", "principalId", "sessionDigest", "sessionEpoch", "tenant", "userId"],
  ) && validMappingFields(record, principalId);
}

function validMappingFields(record: Partial<PrincipalMapping>, principalId: string): boolean {
  return record.principalId === principalId && OPAQUE.test(principalId)
    && typeof record.appId === "string" && APP_ID.test(record.appId)
    && typeof record.appOrigin === "string"
    && publicOrigin(record.appOrigin) === record.appOrigin
    && typeof record.fence === "string" && OPAQUE.test(record.fence)
    && boundedClaim(record.issuer) === record.issuer
    && boundedClaim(record.tenant) === record.tenant
    && typeof record.sessionDigest === "string" && OPAQUE.test(record.sessionDigest)
    && Number.isSafeInteger(record.sessionEpoch) && Number(record.sessionEpoch) >= 1
    && isUserId(record.userId);
}

function validSession(value: unknown): value is HostSession {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Partial<HostSession>;
  return hasExactKeys(
    value as Record<string, unknown>,
    ["active", "fence", "issuer", "principalId", "sessionEpoch", "tenant"],
  )
    && typeof record.active === "boolean"
    && typeof record.fence === "string" && OPAQUE.test(record.fence)
    && boundedClaim(record.issuer) === record.issuer
    && typeof record.principalId === "string" && OPAQUE.test(record.principalId)
    && Number.isSafeInteger(record.sessionEpoch) && Number(record.sessionEpoch) >= 1
    && boundedClaim(record.tenant) === record.tenant;
}

function validExchange(value: unknown): value is Exchange {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Partial<Exchange>;
  const expiresAt = record.expiresAt;
  const resources = record.resources;
  return hasExactKeys(
    value as Record<string, unknown>,
    [
      "appId",
      "appOrigin",
      "expiresAt",
      "fence",
      "issuer",
      "principalId",
      "resources",
      "sessionDigest",
      "sessionEpoch",
      "tenant",
      "userId",
    ],
  )
    && Number.isSafeInteger(expiresAt)
    && hostResources(resources) !== undefined
    && typeof record.principalId === "string"
    && validMappingFields(record, record.principalId);
}

type Locks = Readonly<{ entries: readonly Lock[]; token: string }>;
type Lock = Readonly<{ key: string }>;

async function acquireLocks(store: Kv.Kv, keys: readonly string[]): Promise<Locks | undefined> {
  if (!store.create) return undefined;
  const token = randomToken();
  const locks: Lock[] = [];
  for (const target of [...new Set(keys)].sort()) {
    const key = `lock:${target}`;
    let acquired = false;
    for (let attempt = 0; attempt < 8 && !acquired; attempt += 1) {
      acquired = await store.create(key, token, { ttl: 30 });
      if (!acquired) await scheduler.wait(10 * (attempt + 1));
    }
    if (!acquired) {
      await releaseLocks(store, { entries: locks, token });
      return undefined;
    }
    locks.push({ key });
  }
  try {
    await Promise.all(keys.map((target) => store.set(`fence:${target}`, token)));
    return { entries: locks, token };
  } catch (error) {
    await releaseLocks(store, { entries: locks, token });
    throw error;
  }
}

async function releaseLocks(store: Kv.Kv, locks: Locks): Promise<void> {
  if (!store.take) return;
  await Promise.all(locks.entries.map(async ({ key }) => {
    const current = await store.take?.<unknown>(key);
    if (current === locks.token) return;
    if (current !== undefined) await store.create?.(key, current, { ttl: 30 });
  }));
}

async function readObject(request: Request): Promise<Record<string, unknown> | Response> {
  if (request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase() !== "application/json") {
    return failure(415, "unsupported_media_type");
  }
  const length = request.headers.get("content-length");
  if (length && (!/^\d+$/.test(length) || Number(length) > MAX_BODY_BYTES)) {
    return failure(413, "request_too_large");
  }
  const reader = request.body?.getReader();
  const chunks: Uint8Array[] = [];
  let bytes = 0;
  if (reader) {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > MAX_BODY_BYTES) {
        await reader.cancel();
        return failure(413, "request_too_large");
      }
      chunks.push(value);
    }
  }
  const body = new Uint8Array(bytes);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    const value = JSON.parse(new TextDecoder().decode(body)) as unknown;
    return value && typeof value === "object" && !Array.isArray(value)
      ? value as Record<string, unknown>
      : failure(400, "invalid_json");
  } catch {
    return failure(400, "invalid_json");
  }
}

function hasExactKeys(
  record: Record<string, unknown>,
  expected: readonly string[],
  optional: readonly string[] = [],
): boolean {
  const allowed = new Set(expected);
  const required = new Set(expected.filter((key) => !optional.includes(key)));
  return Object.keys(record).every((key) => allowed.has(key))
    && [...required].every((key) => Object.hasOwn(record, key));
}

function boundedClaim(value: unknown): string | undefined {
  return typeof value === "string" && BOUNDED_CLAIM.test(value) ? value : undefined;
}

function publicOrigin(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  try {
    const url = new URL(value);
    const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname.toLowerCase());
    return url.origin === value && !url.username && !url.password
      && (url.protocol === "https:" || (url.protocol === "http:" && loopback))
      ? url.origin : undefined;
  } catch {
    return undefined;
  }
}

function equalResources(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function constantTimeEqual(left: string, right: string): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    difference |= left.charCodeAt(index) ^ right.charCodeAt(index);
  }
  return difference === 0;
}

async function digest(value: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function keyedDigest(secret: string, value: string): Promise<string> {
  const encoder = new TextEncoder();
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { hash: "SHA-256", name: "HMAC" },
    false,
    ["sign"],
  );
  const bytes = new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(value)));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function randomToken(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function noStoreHeaders(): Record<string, string> {
  return { "cache-control": "no-store", "x-content-type-options": "nosniff" };
}

function methodNotAllowed(method: string): Response {
  return json({ error: "method_not_allowed" }, { status: 405, headers: { allow: method } });
}

function failure(status: number, error: string): Response {
  return json({ error }, { status });
}

function json(value: unknown, init?: ResponseInit): Response {
  const headers = new Headers(init?.headers);
  for (const [name, content] of Object.entries(noStoreHeaders())) headers.set(name, content);
  return Response.json(value, { ...init, headers });
}
