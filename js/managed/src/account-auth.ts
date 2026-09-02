import { DurableObject } from "cloudflare:workers";
import { fetchResponseWithDeadline } from "./deadline";
import { Handler, Kv } from "accounts/server";
import { Address, PublicKey } from "ox";
import {
  isAppToolCatalogDigest,
} from "./app-tool-catalog";
import {
  CONNECTOR_CAPABILITY_IDS,
  type ConnectorCapabilityId,
  type ConnectorConnectionSelection,
} from "./connector-status";

const ACCOUNT_COOKIE = "nanocodex_account";
const LOCAL_PORTABLE_CREDENTIAL_COOKIE = "nanocodex_local_passkey";
const LOCAL_WEBAUTHN_RP_ID = "nanocodex.localhost";
const LOCAL_WEBAUTHN_HOST = /^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)?nanocodex\.localhost$/;
const SESSION_TTL_SECONDS = 30 * 24 * 60 * 60;
const WEBAUTHN_CHALLENGE_TTL_SECONDS = 5 * 60;
const PASSKEY_REAUTH_WINDOW_SECONDS = 365 * 24 * 60 * 60;
const USER_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const API_KEY = /^ncx_live_([A-Za-z0-9_-]{12})_([A-Za-z0-9_-]{43})$/;
const ANONYMOUS_SESSION_TOKEN = /^a_[A-Za-z0-9_-]{43}$/;
const PASSKEY_SESSION_TOKEN = /^[0-9a-f]{64}$/;
const PORTABLE_CREDENTIAL_ID = /^[A-Za-z0-9_-]{1,512}$/;
const PORTABLE_PUBLIC_KEY = /^0x(?:[0-9a-fA-F]{2}){1,1024}$/;
const BASE64_URL = /^[A-Za-z0-9_-]+$/;
const DEFAULT_OWNERSHIP_IO_TIMEOUT_MS = 10_000;
const CONNECT_SERVICE_ORIGIN = "https://nanocodex.internal";
const CONNECT_USER_HEADER = "x-nanocodex-connect-user";
const CONNECT_GRANT_ID_HEADER = "x-nanocodex-connect-grant-id";
const CONNECT_CAPABILITIES_HEADER = "x-nanocodex-connect-capabilities";
const CONNECT_CONNECTORS_HEADER = "x-nanocodex-connect-connectors";
const CONNECT_CONNECTOR_CONNECTIONS_HEADER = "x-nanocodex-connect-connector-connections";
const CONNECT_MCP_IDS_HEADER = "x-nanocodex-connect-mcp-ids";
const CONNECT_APP_TOOL_CATALOG_DIGEST_HEADER = "x-nanocodex-connect-app-tool-catalog-digest";
const SESSION_OWNER_ASSERTION = "x-nanocodex-owner-id";
const SESSION_ORGANIZATION_ASSERTION = "x-nanocodex-session-organization-id";
const SESSION_TEAM_ASSERTION = "x-nanocodex-session-team-id";
const SESSION_AUTHORIZATION_EPOCH_ASSERTION = "x-nanocodex-authorization-epoch";
const SESSION_CAPABILITIES_ASSERTION = "x-nanocodex-capabilities";
const accountSessionKey = (token: string) => `session:${token}`;

export function isUserId(value: unknown): value is string {
  return typeof value === "string" && USER_ID.test(value);
}

export const NonceStorage = Kv.NonceStorage;

export interface AccountAuthEnv {
  NANOCODEX_AUTH: DurableObjectNamespace;
  NANOCODEX_USERS: DurableObjectNamespace<UserAccount>;
  NANOCODEX_API_KEYS: DurableObjectNamespace<ApiKeyRecord>;
  NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY?: string;
  NANOCODEX_ORGANIZATIONS: DurableObjectNamespace<Organization>;
}

export type OrganizationRole = "owner" | "writer" | "reader";

export type OrganizationCapability =
  | "agents:read"
  | "agents:portability"
  | "agents:write"
  | "api_keys:read"
  | "api_keys:write"
  | "history:read"
  | "memory:read"
  | "memory:write"
  | "tools:use"
  | "organization:read"
  | "organization:write";

export type ConnectConnectorId = ConnectorCapabilityId | "chatgpt";

export type ConnectGrantSlice = Readonly<{
  grantId: string;
  connectors: readonly ConnectConnectorId[];
  /** Exact approved account connections. Absent only on legacy capability-level grants. */
  connectorConnections?: ConnectorConnectionSelection;
  mcpIds: readonly string[];
  appToolCatalogDigest?: `0x${string}`;
}>;

const OWNER_CAPABILITIES = [
  "agents:read",
  "agents:portability",
  "agents:write",
  "api_keys:read",
  "api_keys:write",
  "history:read",
  "memory:read",
  "memory:write",
  "tools:use",
  "organization:read",
  "organization:write",
] as const satisfies readonly OrganizationCapability[];

export type Principal = Readonly<{
  kind: "account_session" | "api_key" | "connect_grant";
  userId: string;
  organizationId: string;
  teamId: string;
  role: OrganizationRole;
  subjectId: `user:${string}` | `api_key:${string}`;
  credentialId: string;
  authorizationEpoch: number;
  capabilities: readonly OrganizationCapability[];
  connectGrant?: ConnectGrantSlice;
}>;

export function forwardPrincipalAssertions(headers: Headers, principal: Principal): void {
  headers.set(SESSION_OWNER_ASSERTION, principal.userId);
  headers.set(SESSION_ORGANIZATION_ASSERTION, principal.organizationId);
  headers.set(SESSION_TEAM_ASSERTION, principal.teamId);
  headers.set(SESSION_AUTHORIZATION_EPOCH_ASSERTION, String(principal.authorizationEpoch));
  headers.set(SESSION_CAPABILITIES_ASSERTION, JSON.stringify(principal.capabilities));
  for (const name of [
    CONNECT_USER_HEADER,
    CONNECT_GRANT_ID_HEADER,
    CONNECT_CAPABILITIES_HEADER,
    CONNECT_CONNECTORS_HEADER,
    CONNECT_CONNECTOR_CONNECTIONS_HEADER,
    CONNECT_MCP_IDS_HEADER,
    CONNECT_APP_TOOL_CATALOG_DIGEST_HEADER,
  ]) {
    headers.delete(name);
  }
  if (principal.connectGrant) {
    headers.set(CONNECT_GRANT_ID_HEADER, principal.connectGrant.grantId);
    headers.set(CONNECT_CONNECTORS_HEADER, JSON.stringify(principal.connectGrant.connectors));
    if (principal.connectGrant.connectorConnections !== undefined) {
      headers.set(
        CONNECT_CONNECTOR_CONNECTIONS_HEADER,
        JSON.stringify(principal.connectGrant.connectorConnections),
      );
    }
    headers.set(CONNECT_MCP_IDS_HEADER, JSON.stringify(principal.connectGrant.mcpIds));
    if (principal.connectGrant.appToolCatalogDigest !== undefined) {
      headers.set(CONNECT_APP_TOOL_CATALOG_DIGEST_HEADER, principal.connectGrant.appToolCatalogDigest);
    }
  }
}

type UserRecord = Readonly<{
  id: string;
  organizationId: string;
  persistent: boolean;
  createdAt: number;
  lastAuthenticatedAt: number;
}>;

type OrganizationGrant = Readonly<{
  organizationId: string;
  teamId: string;
  role: OrganizationRole;
  authorizationEpoch: number;
  capabilities: readonly OrganizationCapability[];
}>;

type OrganizationMetadata = Readonly<{
  id: string;
  name: string | null;
  rootTeamId: string;
  authorizationEpoch: number;
  createdAt: number;
  updatedAt: number;
}>;

type TeamRecord = Readonly<{
  id: string;
  organizationId: string;
  parentTeamId: string | null;
  name: string | null;
  createdAt: number;
  updatedAt: number;
}>;

type OrganizationMembership = Readonly<{
  userId: string;
  organizationId: string;
  teamId: string;
  role: OrganizationRole;
  capabilities: readonly OrganizationCapability[];
  createdAt: number;
}>;

const teamStorageKey = (teamId: string) => `team:${teamId}`;
const userMembershipStorageKey = (userId: string) => `membership:user:${userId}`;

type AccountSessionPayload = Readonly<{
  userId: string;
  issuedAt: number;
  expiresAt: number;
}>;

type PortableCredential = Readonly<{
  credentialId: string;
  publicKey: string;
  userId: string;
}>;

export type ApiKeyMetadata = Readonly<{
  id: string;
  label: string;
  prefix: string;
  createdAt: number;
}>;

type ApiKeyBase = ApiKeyMetadata & Readonly<{
  digest: string;
  userId: string;
}>;

type StoredApiKey = ApiKeyBase & Readonly<{
  organizationId: string;
  teamId: string;
  role: OrganizationRole;
  capabilities: readonly OrganizationCapability[];
  authorizationEpoch: number;
}>;

export type AgentSummary = Readonly<{
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  turnCount: number;
}>;

type AgentRegistryRow = Readonly<{
  id: string;
  title: string;
  created_at: number;
  updated_at: number;
  turn_count: number;
  deleted_at: number | null;
}>;

export async function routeAccountRequest(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<Response | undefined> {
  if (url.pathname === "/auth" || url.pathname.startsWith("/auth/")) {
    return json({ error: "not_found" }, { status: 404 });
  }
  if (url.pathname === "/webauthn/portable-credential") {
    if (request.method !== "DELETE") return methodNotAllowed();
    const originFailure = requireBrowserOrigin(request, url);
    if (originFailure) return originFailure;
    const rpId = portableLocalWebAuthnRpId(url);
    if (!rpId || !portableLocalWebAuthnKey(env, url)) {
      return json({ error: "not_found" }, { status: 404 });
    }
    return new Response(null, {
      status: 204,
      headers: {
        "cache-control": "no-store",
        "set-cookie": [
          `${LOCAL_PORTABLE_CREDENTIAL_COOKIE}=`,
          "Path=/",
          `Domain=${rpId}`,
          "Max-Age=0",
          "HttpOnly",
          "SameSite=Lax",
          "Secure",
        ].join("; "),
      },
    });
  }
  if (url.pathname.startsWith("/webauthn/")) {
    const originFailure = requireBrowserOrigin(request, url);
    if (originFailure) return originFailure;
    if (url.pathname === "/webauthn/register/options") {
      const principal = await authenticate(request, env, url);
      if (!principal || principal.kind !== "account_session") return unauthorized();
      const body = await readJson(request);
      if (body instanceof Response) return body;
      request = new Request(request, {
        body: JSON.stringify({
          excludeCredentialIds: body.excludeCredentialIds,
          name: "Nanocodex",
          userId: principal.userId,
        }),
        headers: { ...Object.fromEntries(request.headers), "content-type": "application/json" },
      });
    }
    if (url.pathname === "/webauthn/login/options" && request.method === "POST") {
      return webAuthnLoginOptions(request, env, url);
    }
    if (url.pathname === "/webauthn/login" && request.method === "POST") {
      const targetFailure = await requireSelectedWebAuthnCredential(request, env);
      if (targetFailure) return targetFailure;
      await seedPortableLocalCredential(request, env, url);
    }
    return webAuthnHandler(env, url).fetch(request);
  }
  if (url.pathname === "/v1/me" && request.method === "GET") {
    const resolved = await resolveOrCreateBrowserAccount(request, env, url);
    if (resolved instanceof Response) return resolved;
    const principal = resolved.principal;
    const portableCookie = resolved.persistent
      ? await portableLocalCredentialCookieForSession(request, env, url, principal)
      : undefined;
    const persistentCookie = resolved.persistent
      ? serializePersistentSessionCookie(request, url.protocol)
      : undefined;
    const cookies = [resolved.cookie ?? persistentCookie, portableCookie].filter(
      (cookie): cookie is string => Boolean(cookie),
    );
    const headers = new Headers();
    for (const cookie of cookies) headers.append("set-cookie", cookie);
    return json({
      user: {
        id: principal.userId,
        persistent: resolved.persistent,
      },
      organization: { id: principal.organizationId },
      team: { id: principal.teamId },
      role: principal.role,
      authentication: principal.kind,
    }, cookies.length
      ? { headers }
      : undefined);
  }
  if (url.pathname === "/v1/organization") {
    const principal = await authenticate(request, env, url);
    if (!principal) return unauthorized();
    if (principal.kind !== "account_session") {
      return json({ error: "forbidden" }, { status: 403 });
    }
    const organization = env.NANOCODEX_ORGANIZATIONS.getByName(principal.organizationId);
    if (request.method === "GET") {
      if (!principal.capabilities.includes("organization:read")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      return proxyOrganizationResponse(await organization.fetch("https://organization.internal/metadata"));
    }
    if (request.method === "PATCH") {
      if (!principal.capabilities.includes("organization:write")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      const body = await readJson(request);
      if (body instanceof Response) return body;
      if (Object.keys(body).length !== 1 || !("name" in body)) {
        return json({ error: "invalid_organization" }, { status: 400 });
      }
      const name = organizationName(body.name);
      if (name === undefined) return json({ error: "invalid_organization" }, { status: 400 });
      return proxyOrganizationResponse(await organization.fetch("https://organization.internal/metadata", {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ name }),
      }));
    }
    return methodNotAllowed();
  }
  if (url.pathname === "/v1/api-keys") {
    const principal = request.method === "GET"
      ? await authenticate(request, env, url)
      : await authenticatePersistentAccount(request, env, url);
    if (!principal || principal.kind !== "account_session") return unauthorized();
    if (request.method === "GET") {
      if (!principal.capabilities.includes("api_keys:read")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      return json({ data: await listApiKeys(env, principal.userId) });
    }
    if (request.method === "POST") {
      if (!principal.capabilities.includes("api_keys:write")) {
        return json({ error: "forbidden" }, { status: 403 });
      }
      const originFailure = requireSameOriginMutation(request, url, principal);
      if (originFailure) return originFailure;
      const body = await readJson(request);
      if (body instanceof Response) return body;
      const label = typeof body.label === "string" && body.label.trim()
        ? body.label.trim().slice(0, 120)
        : "API key";
      const created = await createApiKey(env, principal, label);
      return json({ api_key: created.token, key: created.metadata }, { status: 201 });
    }
    return methodNotAllowed();
  }
  const keyMatch = url.pathname.match(/^\/v1\/api-keys\/([A-Za-z0-9_-]{12})$/);
  if (keyMatch) {
    const principal = await authenticatePersistentAccount(request, env, url);
    if (!principal || principal.kind !== "account_session") return unauthorized();
    if (request.method !== "DELETE") return methodNotAllowed();
    if (!principal.capabilities.includes("api_keys:write")) {
      return json({ error: "forbidden" }, { status: 403 });
    }
    const originFailure = requireSameOriginMutation(request, url, principal);
    if (originFailure) return originFailure;
    const deleted = await revokeApiKey(env, principal.userId, keyMatch[1]!);
    return deleted ? new Response(null, { status: 204 }) : json({ error: "not_found" }, { status: 404 });
  }
  return undefined;
}

export async function authenticate(
  request: Request,
  env: AccountAuthEnv,
  url = new URL(request.url),
): Promise<Principal | undefined> {
  const connectUser = request.headers.get(CONNECT_USER_HEADER);
  if (url.origin === CONNECT_SERVICE_ORIGIN && isUserId(connectUser)) {
    const grant = parseConnectGrantAssertions(request.headers);
    if (!grant) return undefined;
    const principal = await resolveUserPrincipal(
      env,
      connectUser,
      `connect_grant:${grant.slice.grantId}`,
    );
    if (!principal || grant.capabilities.some((capability) => (
      !principal.capabilities.includes(capability)
    ))) return undefined;
    return {
      ...principal,
      kind: "connect_grant",
      credentialId: grant.slice.grantId,
      capabilities: grant.capabilities,
      connectGrant: grant.slice,
    };
  }
  const cookie = cookieValue(request, ACCOUNT_COOKIE);
  if (cookie && ANONYMOUS_SESSION_TOKEN.test(cookie)) {
    const session = await readBrowserSession(request, env);
    if (session) {
      return resolveUserPrincipal(
        env,
        session.userId,
        `account_session:${await sha256(cookie)}`,
      );
    }
  } else if (cookie && PASSKEY_SESSION_TOKEN.test(cookie)) {
    const passkey = await webAuthnHandler(env, url).getSession(request);
    const passkeyUserId = passkey?.userId ? decodeUserId(passkey.userId) : undefined;
    if (isUserId(passkeyUserId)) {
      const credentialId = typeof passkey?.credentialId === "string" && passkey.credentialId
        ? passkey.credentialId
        : passkeyUserId;
      return resolveUserPrincipal(env, passkeyUserId, credentialId);
    }
  }
  const authorization = request.headers.get("authorization");
  if (!authorization?.startsWith("Bearer ")) return undefined;
  const token = authorization.slice("Bearer ".length);
  if (!API_KEY.test(token)) return undefined;
  const digest = await sha256(token);
  const stub = env.NANOCODEX_API_KEYS.getByName(digest);
  const response = await stub.fetch("https://api-key.internal/resolve");
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  const record = await response.json<StoredApiKey>();
  if (record.digest !== digest || !isStoredApiKey(record)) return undefined;
  const account = await readAccount(env, record.userId);
  if (!account || account.organizationId !== record.organizationId) {
    return undefined;
  }
  const grant = await resolveOrganizationGrant(env, account);
  if (!grant
    || grant.teamId !== record.teamId
    || grant.authorizationEpoch !== record.authorizationEpoch
    || organizationRoleRank(record.role) > organizationRoleRank(grant.role)
    || record.capabilities.some((capability) => !grant.capabilities.includes(capability))) {
    return undefined;
  }
  return {
    kind: "api_key",
    userId: record.userId,
    organizationId: record.organizationId,
    teamId: record.teamId,
    role: record.role,
    subjectId: `api_key:${record.id}`,
    credentialId: record.id,
    authorizationEpoch: record.authorizationEpoch,
    capabilities: record.capabilities,
  };
}

async function resolveUserPrincipal(
  env: AccountAuthEnv,
  userId: string,
  credentialId: string,
): Promise<Principal | undefined> {
  const account = await readAccount(env, userId);
  if (!account) return undefined;
  const grant = await resolveOrganizationGrant(env, account);
  if (!grant) return undefined;
  return {
    kind: "account_session",
    userId,
    ...grant,
    subjectId: `user:${userId}`,
    credentialId,
  };
}

export async function authenticatePersistentAccount(
  request: Request,
  env: AccountAuthEnv,
  url = new URL(request.url),
): Promise<Principal | undefined> {
  const principal = await authenticate(request, env, url);
  if (!principal || principal.kind !== "account_session") return undefined;
  const account = await readAccount(env, principal.userId);
  return account?.persistent === true ? principal : undefined;
}

export type PersistentPasskeyAccount = Readonly<{
  accountAddress: string;
  credentialId: string;
  principal: Principal;
  publicKey: string;
}>;

export async function authenticatePersistentPasskeyAccount(
  request: Request,
  env: AccountAuthEnv,
  url = new URL(request.url),
): Promise<PersistentPasskeyAccount | undefined> {
  const principal = await authenticatePersistentAccount(request, env, url);
  if (!principal) return undefined;
  const session = await webAuthnHandler(env, url).getSession(request);
  const userId = session?.userId ? decodeUserId(session.userId) : undefined;
  if (!session
    || userId !== principal.userId
    || typeof session.credentialId !== "string"
    || !PORTABLE_CREDENTIAL_ID.test(session.credentialId)
    || typeof session.publicKey !== "string"
    || !PORTABLE_PUBLIC_KEY.test(session.publicKey)) return undefined;
  try {
    const publicKey = PublicKey.fromHex(session.publicKey as `0x${string}`);
    if (publicKey.y === undefined) return undefined;
    return {
      accountAddress: Address.fromPublicKey(publicKey).toLowerCase(),
      credentialId: session.credentialId,
      principal,
      publicKey: session.publicKey,
    };
  } catch {
    return undefined;
  }
}

export function requireSameOriginMutation(
  request: Request,
  url: URL,
  principal: Principal,
): Response | undefined {
  if (principal.kind !== "account_session") return undefined;
  return request.headers.get("origin") === url.origin
    ? undefined
    : json({ error: "forbidden_origin" }, { status: 403 });
}

export async function listAgents(env: AccountAuthEnv, userId: string): Promise<AgentSummary[]> {
  const response = await env.NANOCODEX_USERS.getByName(userId).fetch("https://user.internal/agents");
  if (!response.ok) throw new Error("agent listing failed");
  return response.json<AgentSummary[]>();
}

export async function attachAgent(
  env: AccountAuthEnv,
  userId: string,
  agentId: string,
  timeoutMs = DEFAULT_OWNERSHIP_IO_TIMEOUT_MS,
): Promise<void> {
  await fetchResponseWithDeadline(
    env.NANOCODEX_USERS.getByName(userId),
    "https://user.internal/agents",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agentId }),
    },
    timeoutMs,
    "agent attachment",
    (response) => {
      if (!response.ok) throw new Error("agent attachment failed");
    },
  );
}

export async function recordAgentActivity(
  env: AccountAuthEnv,
  userId: string,
  agentId: string,
  summary: Readonly<{ title: string; turnCount: number }>,
): Promise<void> {
  let failure: unknown;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      const response = await env.NANOCODEX_USERS.getByName(userId).fetch(
        `https://user.internal/agents/${agentId}/activity`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(summary),
        },
      );
      if (!response.ok) throw new Error(`agent activity update failed with HTTP ${response.status}`);
      await response.body?.cancel();
      return;
    } catch (error) {
      failure = error;
      if (attempt < 2) await scheduler.wait(10 * 2 ** attempt);
    }
  }
  throw failure;
}

export async function detachAgent(
  env: AccountAuthEnv,
  userId: string,
  agentId: string,
  timeoutMs = DEFAULT_OWNERSHIP_IO_TIMEOUT_MS,
): Promise<void> {
  await fetchResponseWithDeadline(
    env.NANOCODEX_USERS.getByName(userId),
    `https://user.internal/agents/${agentId}`,
    { method: "DELETE" },
    timeoutMs,
    "agent detachment",
    (response) => {
      if (!response.ok && response.status !== 404) throw new Error("agent detachment failed");
    },
  );
}

function webAuthnHandler(env: AccountAuthEnv, url: URL) {
  return Handler.webAuthn({
    cookieName: ACCOUNT_COOKIE,
    kv: authStore(env, "webauthn"),
    origin: url.origin,
    path: "/webauthn",
    rpId: portableLocalWebAuthnRpId(url) ?? url.hostname,
    ttl: { session: SESSION_TTL_SECONDS },
    onRegister: async ({ credentialId, publicKey, request, userId }) => {
      const decoded = userId ? decodeUserId(userId) : undefined;
      const current = await readBrowserSession(request, env);
      if (!decoded || !current || decoded !== current.userId) {
        throw new Error("passkey identity does not match this browser session");
      }
      await ensureAccount(env, decoded, true);
      const anonymousToken = cookieValue(request, ACCOUNT_COOKIE);
      if (anonymousToken) {
        await authStore(env, "account").delete(accountSessionKey(anonymousToken));
      }
      return portableLocalCredentialResponse(env, url, {
        credentialId,
        publicKey,
        userId: decoded,
      });
    },
    onAuthenticate: async ({ credentialId, publicKey, userId }) => {
      const decoded = userId ? decodeUserId(userId) : undefined;
      if (!isUserId(decoded)) throw new Error("unknown passkey identity");
      if (portableLocalWebAuthnKey(env, url)) {
        await ensureAccount(env, decoded, true);
      }
      return portableLocalCredentialResponse(env, url, {
        credentialId,
        publicKey,
        userId: decoded,
      });
    },
  });
}

async function webAuthnLoginOptions(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<Response> {
  const body = await readJson(request);
  if (body instanceof Response) return body;
  const requested = requestedWebAuthnCredentialIds(body);
  if (requested instanceof Response) return requested;

  if (requested) {
    const portable = await readPortableLocalCredential(request, env, url);
    const store = authStore(env, "webauthn");
    for (const credentialId of requested) {
      if (portable?.credentialId === credentialId) continue;
      const credential = await store.get<unknown>(`credential:${credentialId}`);
      if (!isStoredWebAuthnCredential(credential)) {
        return json({ error: "unknown credential" }, { status: 400 });
      }
    }
  }

  const headers = new Headers(request.headers);
  headers.set("content-type", "application/json");
  const response = await webAuthnHandler(env, url).fetch(new Request(request, {
    body: JSON.stringify(body),
    headers,
  }));
  if (!response.ok || !requested) return response;

  const payload = await response.clone().json<unknown>().catch(() => undefined);
  const challenge = webAuthnOptionsChallenge(payload);
  if (!challenge) return json({ error: "invalid_webauthn_options" }, { status: 500 });
  await authStore(env, "webauthn").set(
    `login-target:${challenge}`,
    requested,
    { ttl: WEBAUTHN_CHALLENGE_TTL_SECONDS },
  );
  return response;
}

function requestedWebAuthnCredentialIds(
  body: Record<string, unknown>,
): readonly string[] | Response | undefined {
  const value = body.credentialId ?? body.allowCredentialIds;
  if (value === undefined) return undefined;
  const ids = typeof value === "string"
    ? [value]
    : Array.isArray(value) ? value : [];
  if (ids.length === 0
    || ids.some((credentialId) => typeof credentialId !== "string"
      || !PORTABLE_CREDENTIAL_ID.test(credentialId))) {
    return json({ error: "invalid_credential_target" }, { status: 400 });
  }
  return [...new Set(ids as string[])];
}

function isStoredWebAuthnCredential(value: unknown): boolean {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const credential = value as Record<string, unknown>;
  return typeof credential.publicKey === "string"
    && PORTABLE_PUBLIC_KEY.test(credential.publicKey)
    && typeof credential.userId === "string"
    && isUserId(decodeUserId(credential.userId));
}

function webAuthnOptionsChallenge(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  const options = (value as Record<string, unknown>).options;
  if (typeof options !== "object" || options === null || Array.isArray(options)) return undefined;
  const publicKey = (options as Record<string, unknown>).publicKey;
  if (typeof publicKey !== "object" || publicKey === null || Array.isArray(publicKey)) return undefined;
  const challenge = (publicKey as Record<string, unknown>).challenge;
  return typeof challenge === "string" && BASE64_URL.test(challenge) ? challenge : undefined;
}

async function requireSelectedWebAuthnCredential(
  request: Request,
  env: AccountAuthEnv,
): Promise<Response | undefined> {
  let body: unknown;
  try {
    body = await request.clone().json();
  } catch {
    return undefined;
  }
  if (typeof body !== "object" || body === null || Array.isArray(body)) return undefined;
  const assertion = body as Record<string, unknown>;
  if (typeof assertion.id !== "string") return undefined;
  const metadata = assertion.metadata;
  if (typeof metadata !== "object" || metadata === null || Array.isArray(metadata)) return undefined;
  const clientDataJSON = (metadata as Record<string, unknown>).clientDataJSON;
  if (typeof clientDataJSON !== "string") return undefined;

  let challenge: unknown;
  try {
    const clientData = JSON.parse(clientDataJSON) as unknown;
    challenge = typeof clientData === "object" && clientData !== null && !Array.isArray(clientData)
      ? (clientData as Record<string, unknown>).challenge
      : undefined;
  } catch {
    return undefined;
  }
  if (typeof challenge !== "string" || !BASE64_URL.test(challenge)) return undefined;
  const selected = await authStore(env, "webauthn").get<unknown>(`login-target:${challenge}`);
  if (!Array.isArray(selected)) return undefined;
  if (!selected.includes(assertion.id)) {
    return json({ error: "selected_credential_mismatch" }, { status: 400 });
  }
  return undefined;
}

async function seedPortableLocalCredential(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<void> {
  if (!portableLocalWebAuthnKey(env, url)) return;
  let body: unknown;
  try {
    body = await request.clone().json();
  } catch {
    return;
  }
  if (typeof body !== "object" || body === null || Array.isArray(body)) return;
  const assertedCredentialId = (body as Record<string, unknown>).id;
  if (typeof assertedCredentialId !== "string") return;
  const credential = await readPortableLocalCredential(request, env, url);
  if (!credential || credential.credentialId !== assertedCredentialId) return;
  await authStore(env, "webauthn").set(`credential:${credential.credentialId}`, {
    publicKey: credential.publicKey,
    userId: encodeUserId(credential.userId),
  });
}

async function portableLocalCredentialCookieForSession(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
  principal: Principal,
): Promise<string | undefined> {
  if (principal.kind !== "account_session" || !portableLocalWebAuthnKey(env, url)) return undefined;
  const session = await webAuthnHandler(env, url).getSession(request);
  const userId = session?.userId ? decodeUserId(session.userId) : undefined;
  if (!session || userId !== principal.userId) return undefined;
  return serializePortableLocalCredentialCookie(env, url, {
    credentialId: session.credentialId,
    publicKey: session.publicKey,
    userId,
  });
}

async function portableLocalCredentialResponse(
  env: AccountAuthEnv,
  url: URL,
  credential: PortableCredential,
): Promise<Response> {
  const cookie = await serializePortableLocalCredentialCookie(env, url, credential);
  return new Response(null, cookie ? { headers: { "set-cookie": cookie } } : undefined);
}

async function serializePortableLocalCredentialCookie(
  env: AccountAuthEnv,
  url: URL,
  credential: PortableCredential,
): Promise<string | undefined> {
  const secret = portableLocalWebAuthnKey(env, url);
  if (!secret || !isPortableCredential(credential)) return undefined;
  const payload = encodeBase64Url(new TextEncoder().encode(JSON.stringify(credential)));
  const signature = await portableCredentialSignature(secret, payload);
  return [
    `${LOCAL_PORTABLE_CREDENTIAL_COOKIE}=${payload}.${encodeBase64Url(signature)}`,
    "Path=/",
    `Domain=${portableLocalWebAuthnRpId(url)}`,
    `Max-Age=${SESSION_TTL_SECONDS}`,
    "HttpOnly",
    "SameSite=Lax",
    "Secure",
  ].join("; ");
}

async function readPortableLocalCredential(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<PortableCredential | undefined> {
  const secret = portableLocalWebAuthnKey(env, url);
  if (!secret) return undefined;
  const encoded = rawCookie(request, LOCAL_PORTABLE_CREDENTIAL_COOKIE).value;
  if (!encoded || encoded.length > 4_096) return undefined;
  const parts = encoded.split(".");
  if (parts.length !== 2) return undefined;
  const [payload, encodedSignature] = parts as [string, string];
  const signature = decodeBase64Url(encodedSignature);
  if (!payload || !signature || signature.byteLength !== 32) return undefined;
  const key = await portableCredentialHmacKey(secret, ["verify"]);
  const valid = await crypto.subtle.verify("HMAC", key, signature, new TextEncoder().encode(payload));
  if (!valid) return undefined;
  const decoded = decodeBase64Url(payload);
  if (!decoded) return undefined;
  try {
    const credential = JSON.parse(new TextDecoder().decode(decoded)) as unknown;
    return isPortableCredential(credential) ? credential : undefined;
  } catch {
    return undefined;
  }
}

function isPortableCredential(value: unknown): value is PortableCredential {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).sort();
  return keys.length === 3
    && keys[0] === "credentialId"
    && keys[1] === "publicKey"
    && keys[2] === "userId"
    && typeof record.credentialId === "string"
    && PORTABLE_CREDENTIAL_ID.test(record.credentialId)
    && typeof record.publicKey === "string"
    && PORTABLE_PUBLIC_KEY.test(record.publicKey)
    && isUserId(record.userId);
}

function portableLocalWebAuthnRpId(url: URL): string | undefined {
  return (url.protocol === "http:" || url.protocol === "https:")
    && LOCAL_WEBAUTHN_HOST.test(url.hostname.toLowerCase())
    ? LOCAL_WEBAUTHN_RP_ID
    : undefined;
}

function portableLocalWebAuthnKey(env: AccountAuthEnv, url: URL): string | undefined {
  const secret = env.NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY;
  return portableLocalWebAuthnRpId(url) && typeof secret === "string" && secret.length > 0
    ? secret
    : undefined;
}

async function portableCredentialSignature(secret: string, payload: string): Promise<Uint8Array> {
  const key = await portableCredentialHmacKey(secret, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(payload)));
}

function portableCredentialHmacKey(
  secret: string,
  usages: ("sign" | "verify")[],
): Promise<CryptoKey> {
  return crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    usages,
  );
}

function encodeUserId(value: string): string {
  return encodeBase64Url(new TextEncoder().encode(value));
}

function encodeBase64Url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeBase64Url(value: string): Uint8Array | undefined {
  if (!value || !BASE64_URL.test(value)) return undefined;
  try {
    const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
    const padded = base64.padEnd(Math.ceil(base64.length / 4) * 4, "=");
    return Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
  } catch {
    return undefined;
  }
}

function authStore(env: AccountAuthEnv, name: string): Kv.Kv {
  const namespace = env.NANOCODEX_AUTH as unknown as Parameters<typeof Kv.durableObject>[0];
  return Kv.durableObject(namespace, { name });
}

export async function ensureAccount(
  env: AccountAuthEnv,
  userId: string,
  persistent: boolean,
): Promise<void> {
  if (!isUserId(userId)) {
    throw new Error("invalid account identity");
  }
  const response = await env.NANOCODEX_USERS.getByName(userId).fetch(
    "https://user.internal/account",
    {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: userId, persistent }),
    },
  );
  if (response.ok) {
    await response.body?.cancel();
    return;
  }
  const status = response.status;
  await response.body?.cancel();
  if (status === 409) {
    const current = await readAccount(env, userId);
    if (current?.id === userId && (current.persistent || !persistent)) return;
  }
  throw new Error("account provisioning failed");
}

async function readAccount(env: AccountAuthEnv, userId: string): Promise<UserRecord | undefined> {
  const response = await env.NANOCODEX_USERS.getByName(userId).fetch("https://user.internal/account");
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  const record = await response.json<UserRecord>();
  return isUserRecord(record) ? record : undefined;
}

export async function isPersistentAccount(env: AccountAuthEnv, userId: string): Promise<boolean> {
  return (await readAccount(env, userId))?.persistent === true;
}

async function resolveOrganizationGrant(
  env: AccountAuthEnv,
  account: UserRecord,
): Promise<OrganizationGrant | undefined> {
  const response = await env.NANOCODEX_ORGANIZATIONS.getByName(account.organizationId).fetch(
    `https://organization.internal/resolve?userId=${encodeURIComponent(account.id)}`,
  );
  if (!response.ok) {
    await response.body?.cancel();
    return undefined;
  }
  const grant = await response.json<OrganizationGrant>();
  if (!isOrganizationGrant(grant)
    || grant.organizationId !== account.organizationId) {
    return undefined;
  }
  return grant;
}

async function resolveOrCreateBrowserAccount(
  request: Request,
  env: AccountAuthEnv,
  url: URL,
): Promise<{ principal: Principal; persistent: boolean; cookie?: string } | Response> {
  const principal = await authenticate(request, env, url);
  if (principal) {
    if (principal.kind === "account_session") {
      const account = await readAccount(env, principal.userId);
      if (!account) throw new Error("browser account is unavailable");
      return { principal, persistent: account.persistent };
    }
    const account = await readAccount(env, principal.userId);
    if (!account) throw new Error("API key account is unavailable");
    return { principal, persistent: account.persistent };
  }

  // An explicit but invalid credential must not silently become a fresh
  // browser account. Cookie-free browser bootstrap is only for requests that
  // did not present either account or bearer authentication.
  if (request.headers.has("authorization")) return unauthorized();
  if (hasCookie(request, ACCOUNT_COOKIE)) {
    const token = cookieValue(request, ACCOUNT_COOKIE);
    const reauthenticationRequired = Boolean(token && PASSKEY_SESSION_TOKEN.test(token));
    return json({
      error: reauthenticationRequired
        ? "reauthentication_required"
        : "invalid_session",
    }, {
      status: 401,
      headers: {
        "set-cookie": reauthenticationRequired
          ? accountCookie(token!, PASSKEY_REAUTH_WINDOW_SECONDS, new URL(request.url).protocol)
          : clearAccountCookie(new URL(request.url).protocol),
      },
    });
  }

  const userId = crypto.randomUUID();
  const issuedAt = Math.floor(Date.now() / 1_000);
  const token = `a_${randomBase64Url(32)}`;
  await Promise.all([
    ensureAccount(env, userId, false),
    authStore(env, "account").set(accountSessionKey(token), {
      userId,
      issuedAt,
      expiresAt: issuedAt + SESSION_TTL_SECONDS,
    } satisfies AccountSessionPayload, { ttl: SESSION_TTL_SECONDS }),
  ]);
  const createdPrincipal = await resolveUserPrincipal(
    env,
    userId,
    `account_session:${await sha256(token)}`,
  );
  if (!createdPrincipal) throw new Error("account organization provisioning failed");
  return {
    principal: createdPrincipal,
    persistent: false,
    cookie: serializeAccountCookie(token, new URL(request.url).protocol),
  };
}

async function readBrowserSession(
  request: Request,
  env: AccountAuthEnv,
): Promise<AccountSessionPayload | undefined> {
  const token = cookieValue(request, ACCOUNT_COOKIE);
  if (!token) return undefined;
  const session = await authStore(env, "account").get<AccountSessionPayload>(accountSessionKey(token));
  if (!session || !isUserId(session.userId) || session.expiresAt <= Date.now() / 1_000) {
    return undefined;
  }
  return session;
}

function decodeUserId(value: string): string | undefined {
  try {
    const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
    const padded = base64.padEnd(Math.ceil(base64.length / 4) * 4, "=");
    return new TextDecoder().decode(Uint8Array.from(atob(padded), (character) => character.charCodeAt(0)));
  } catch {
    return undefined;
  }
}

function cookieValue(request: Request, name: string): string | undefined {
  const cookie = rawCookie(request, name);
  return cookie.present
    && (ANONYMOUS_SESSION_TOKEN.test(cookie.value) || PASSKEY_SESSION_TOKEN.test(cookie.value))
    ? cookie.value
    : undefined;
}

function hasCookie(request: Request, name: string): boolean {
  return rawCookie(request, name).present;
}

function rawCookie(request: Request, name: string): { present: boolean; value: string } {
  for (const part of request.headers.get("cookie")?.split(";") ?? []) {
    const separator = part.indexOf("=");
    if (separator >= 0 && part.slice(0, separator).trim() === name) {
      return { present: true, value: part.slice(separator + 1).trim() };
    }
  }
  return { present: false, value: "" };
}

function serializeAccountCookie(token: string, protocol: string): string {
  return accountCookie(token, SESSION_TTL_SECONDS, protocol);
}

function serializePersistentSessionCookie(request: Request, protocol: string): string | undefined {
  const token = cookieValue(request, ACCOUNT_COOKIE);
  return token && PASSKEY_SESSION_TOKEN.test(token)
    ? accountCookie(token, PASSKEY_REAUTH_WINDOW_SECONDS, protocol)
    : undefined;
}

function clearAccountCookie(protocol: string): string {
  return accountCookie("", 0, protocol);
}

function accountCookie(value: string, maxAge: number, protocol: string): string {
  return [
    `${ACCOUNT_COOKIE}=${value}`,
    "Path=/",
    `Max-Age=${maxAge}`,
    "HttpOnly",
    "SameSite=Lax",
    ...(protocol === "https:" ? ["Secure"] : []),
  ].join("; ");
}

function requireBrowserOrigin(request: Request, url: URL): Response | undefined {
  return request.headers.get("origin") === url.origin
    ? undefined
    : json({ error: "forbidden_origin" }, { status: 403 });
}

async function listApiKeys(env: AccountAuthEnv, userId: string): Promise<ApiKeyMetadata[]> {
  const response = await env.NANOCODEX_USERS.getByName(userId).fetch("https://user.internal/api-keys");
  if (!response.ok) throw new Error("API key listing failed");
  return response.json<ApiKeyMetadata[]>();
}

export type ApiKeyMaterial = Readonly<{ id: string; token: string; createdAt: number }>;

export function newApiKeyMaterial(): ApiKeyMaterial {
  const id = randomBase64Url(9);
  return { id, token: `ncx_live_${id}_${randomBase64Url(32)}`, createdAt: Date.now() };
}

export async function createApiKey(
  env: AccountAuthEnv,
  principal: Principal,
  label: string,
  material = newApiKeyMaterial(),
): Promise<{ token: string; metadata: ApiKeyMetadata }> {
  const { id, token, createdAt } = material;
  if (token.match(API_KEY)?.[1] !== id) throw new Error("invalid API key material");
  const digest = await sha256(token);
  const metadata: ApiKeyMetadata = {
    id,
    label,
    prefix: `ncx_live_${id}`,
    createdAt,
  };
  const key = env.NANOCODEX_API_KEYS.getByName(digest);
  const record = {
    ...metadata,
    digest,
    userId: principal.userId,
    organizationId: principal.organizationId,
    teamId: principal.teamId,
    role: principal.role,
    capabilities: principal.capabilities,
    authorizationEpoch: principal.authorizationEpoch,
  } satisfies StoredApiKey;
  const initialized = await key.fetch("https://api-key.internal/record", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(record),
  });
  const inserted = initialized.status === 201;
  if (!inserted) {
    await initialized.body?.cancel();
    const existing = material === undefined ? undefined : await key.fetch("https://api-key.internal/resolve");
    if (!existing?.ok) {
      await existing?.body?.cancel();
      throw new Error("API key creation failed");
    }
    const existingRecord = await existing.json<unknown>();
    if (!sameStoredApiKey(existingRecord, record)) throw new Error("API key creation conflict");
  } else {
    await initialized.body?.cancel();
  }
  const attached = await env.NANOCODEX_USERS.getByName(principal.userId).fetch(
    "https://user.internal/api-keys",
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...metadata, digest }),
    },
  );
  if (!attached.ok) {
    if (inserted) await key.fetch("https://api-key.internal/record", { method: "DELETE" });
    throw new Error("API key attachment failed");
  }
  await attached.body?.cancel();
  return { token, metadata };
}

export async function revokeApiKey(
  env: AccountAuthEnv,
  userId: string,
  id: string,
  token?: string,
): Promise<boolean> {
  const account = env.NANOCODEX_USERS.getByName(userId);
  let digest: string;
  if (token !== undefined) {
    if (token.match(API_KEY)?.[1] !== id) throw new Error("invalid API key material");
    digest = await sha256(token);
  } else {
    const found = await account.fetch(`https://user.internal/api-keys/${id}`);
    if (!found.ok) {
      await found.body?.cancel();
      return false;
    }
    digest = (await found.json<ApiKeyMetadata & { digest: string }>()).digest;
  }
  const deleted = await env.NANOCODEX_API_KEYS.getByName(digest).fetch(
    "https://api-key.internal/record",
    { method: "DELETE" },
  );
  if (!deleted.ok) {
    await deleted.body?.cancel();
    throw new Error("API key revocation failed");
  }
  await deleted.body?.cancel();
  const detached = await account.fetch(`https://user.internal/api-keys/${id}`, { method: "DELETE" });
  if (!detached.ok && detached.status !== 404) {
    await detached.body?.cancel();
    throw new Error("API key detachment failed");
  }
  await detached.body?.cancel();
  return true;
}

export class UserAccount extends DurableObject<AccountAuthEnv> {
  constructor(ctx: DurableObjectState, env: AccountAuthEnv) {
    super(ctx, env);
    ctx.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS agent_registry (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        turn_count INTEGER NOT NULL DEFAULT 0 CHECK (turn_count >= 0),
        deleted_at INTEGER
      );
      CREATE INDEX IF NOT EXISTS agent_registry_active_created
        ON agent_registry (created_at, id) WHERE deleted_at IS NULL;
    `);
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/account") {
      if (request.method === "PUT") {
        const body = await request.json<{ id?: unknown; persistent?: unknown }>();
        const id = typeof body.id === "string" ? body.id.toLowerCase() : "";
        if (!isUserId(id) || typeof body.persistent !== "boolean") {
          return json({ error: "invalid_account" }, { status: 400 });
        }
        const now = Date.now();
        const current = await this.ctx.storage.get<UserRecord>("account");
        if (current && (!isUserRecord(current) || current.id !== id)) {
          return json({ error: "invalid_account_state" }, { status: 409 });
        }
        const record: UserRecord = {
          id,
          organizationId: current?.organizationId ?? crypto.randomUUID(),
          persistent: current?.persistent === true || body.persistent,
          createdAt: current?.createdAt ?? now,
          lastAuthenticatedAt: now,
        };
        await this.ctx.storage.put("account", record);
        const rootTeamId = crypto.randomUUID();
        const initialized = await this.env.NANOCODEX_ORGANIZATIONS.getByName(record.organizationId).fetch(
          "https://organization.internal/initialize",
          {
            method: "PUT",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              organizationId: record.organizationId,
              rootTeamId,
              ownerUserId: record.id,
            }),
          },
        );
        if (!initialized.ok) {
          await initialized.body?.cancel();
          return json({ error: "organization_provisioning_failed" }, { status: 500 });
        }
        await initialized.body?.cancel();
        return json(record);
      }
      if (request.method === "GET") {
        const record = await this.ctx.storage.get<UserRecord>("account");
        return record ? json(record) : json({ error: "not_found" }, { status: 404 });
      }
    }
    if (url.pathname === "/api-keys") {
      const keys = await this.ctx.storage.get<Record<string, ApiKeyMetadata & { digest: string }>>("apiKeys") ?? {};
      if (request.method === "GET") {
        return json(Object.values(keys).map(({ digest: _digest, ...metadata }) => metadata));
      }
      if (request.method === "POST") {
        const metadata = await request.json<ApiKeyMetadata & { digest?: unknown }>();
        if (!/^[A-Za-z0-9_-]{12}$/.test(metadata.id) || typeof metadata.digest !== "string") {
          return json({ error: "invalid_api_key" }, { status: 400 });
        }
        const current = keys[metadata.id];
        if (current && current.digest !== metadata.digest) {
          return json({ error: "conflict" }, { status: 409 });
        }
        keys[metadata.id] = metadata as ApiKeyMetadata & { digest: string };
        await this.ctx.storage.put("apiKeys", keys);
        return new Response(null, { status: 204 });
      }
    }
    const keyMatch = url.pathname.match(/^\/api-keys\/([A-Za-z0-9_-]{12})$/);
    if (keyMatch) {
      const keys = await this.ctx.storage.get<Record<string, ApiKeyMetadata & { digest: string }>>("apiKeys") ?? {};
      const record = keys[keyMatch[1]!];
      if (!record) return json({ error: "not_found" }, { status: 404 });
      if (request.method === "GET") return json(record);
      if (request.method === "DELETE") {
        delete keys[keyMatch[1]!];
        await this.ctx.storage.put("apiKeys", keys);
        return new Response(null, { status: 204 });
      }
    }
    if (url.pathname === "/agents") {
      if (request.method === "GET") {
        return json(this.ctx.storage.sql.exec<AgentRegistryRow>(
          `SELECT id, title, created_at, updated_at, turn_count, deleted_at
           FROM agent_registry
           WHERE deleted_at IS NULL
           ORDER BY created_at, id`,
        ).toArray().map(agentSummary));
      }
      if (request.method === "POST") {
        const body = await request.json<{ agentId?: unknown }>();
        const agentId = typeof body.agentId === "string" ? body.agentId : "";
        if (!/^[0-9a-f-]{36}$/.test(agentId)) {
          return json({ error: "invalid_agent" }, { status: 400 });
        }
        const existing = this.ctx.storage.sql.exec<{ deleted_at: number | null }>(
          "SELECT deleted_at FROM agent_registry WHERE id = ?",
          agentId,
        ).toArray()[0];
        if (existing?.deleted_at !== null && existing !== undefined) {
          return json({ error: "agent_deleted" }, { status: 410 });
        }
        if (!existing) {
          const now = Date.now();
          this.ctx.storage.sql.exec(
            `INSERT INTO agent_registry
               (id, title, created_at, updated_at, turn_count, deleted_at)
             VALUES (?, '', ?, ?, 0, NULL)`,
            agentId,
            now,
            now,
          );
        }
        return new Response(null, { status: 204 });
      }
    }
    const activityMatch = url.pathname.match(/^\/agents\/([0-9a-f-]{36})\/activity$/);
    if (activityMatch && request.method === "POST") {
      const agentId = activityMatch[1]!;
      const body = await request.json<{ title?: unknown; turnCount?: unknown }>();
      const title = typeof body.title === "string" ? body.title.trim().slice(0, 56) : "";
      const turnCount = Number.isSafeInteger(body.turnCount) && Number(body.turnCount) >= 0
        ? Number(body.turnCount) : undefined;
      if (turnCount === undefined) return json({ error: "invalid_activity" }, { status: 400 });
      const updated = this.ctx.storage.sql.exec(
        `UPDATE agent_registry
         SET title = CASE WHEN title = '' THEN ? ELSE title END,
             updated_at = ?,
             turn_count = MAX(turn_count, ?)
         WHERE id = ? AND deleted_at IS NULL`,
        title,
        Date.now(),
        turnCount,
        agentId,
      );
      if (updated.rowsWritten === 0) return json({ error: "not_found" }, { status: 404 });
      return new Response(null, { status: 204 });
    }
    const agentMatch = url.pathname.match(/^\/agents\/([0-9a-f-]{36})$/);
    if (agentMatch && request.method === "DELETE") {
      const agentId = agentMatch[1]!;
      const now = Date.now();
      this.ctx.storage.sql.exec(
        `INSERT INTO agent_registry
           (id, title, created_at, updated_at, turn_count, deleted_at)
         VALUES (?, '', ?, ?, 0, ?)
         ON CONFLICT(id) DO UPDATE SET deleted_at = COALESCE(agent_registry.deleted_at, excluded.deleted_at)`,
        agentId,
        now,
        now,
        now,
      );
      return new Response(null, { status: 204 });
    }
    return json({ error: "not_found" }, { status: 404 });
  }

}

function agentSummary(row: AgentRegistryRow): AgentSummary {
  return {
    id: row.id,
    title: row.title,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    turnCount: row.turn_count,
  };
}

export class Organization extends DurableObject<AccountAuthEnv> {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/initialize" && request.method === "PUT") {
      const body = await request.json<{
        organizationId?: unknown;
        rootTeamId?: unknown;
        ownerUserId?: unknown;
      }>();
      if (!isUuid(body.organizationId) || !isUuid(body.rootTeamId) || !isUserId(body.ownerUserId)) {
        return json({ error: "invalid_organization" }, { status: 400 });
      }
      const existing = await this.ctx.storage.get<OrganizationMetadata>("metadata");
      if (existing) {
        const [membership, rootTeam] = await Promise.all([
          this.ctx.storage.get<OrganizationMembership>(userMembershipStorageKey(body.ownerUserId)),
          this.ctx.storage.get<TeamRecord>(teamStorageKey(existing.rootTeamId)),
        ]);
        if (!isOrganizationMetadata(existing)
          || !isOrganizationMembership(membership)
          || !isTeamRecord(rootTeam)
          || existing.id !== body.organizationId
          || rootTeam.id !== existing.rootTeamId
          || rootTeam.organizationId !== existing.id
          || membership.userId !== body.ownerUserId
          || membership.organizationId !== existing.id
          || membership.teamId !== existing.rootTeamId) {
          return json({ error: "conflict" }, { status: 409 });
        }
        return json(await organizationMetadataView(this.ctx.storage, existing));
      }
      const now = Date.now();
      const metadata: OrganizationMetadata = {
        id: body.organizationId,
        name: null,
        rootTeamId: body.rootTeamId,
        authorizationEpoch: 1,
        createdAt: now,
        updatedAt: now,
      };
      const rootTeam: TeamRecord = {
        id: body.rootTeamId,
        organizationId: body.organizationId,
        parentTeamId: null,
        name: null,
        createdAt: now,
        updatedAt: now,
      };
      const ownerMembership: OrganizationMembership = {
        userId: body.ownerUserId,
        organizationId: body.organizationId,
        teamId: body.rootTeamId,
        role: "owner",
        capabilities: OWNER_CAPABILITIES,
        createdAt: now,
      };
      await this.ctx.storage.put({
        metadata,
        [teamStorageKey(rootTeam.id)]: rootTeam,
        [userMembershipStorageKey(ownerMembership.userId)]: ownerMembership,
      });
      return json(await organizationMetadataView(this.ctx.storage, metadata), { status: 201 });
    }
    if (url.pathname === "/resolve" && (request.method === "GET" || request.method === "POST")) {
      let userId: unknown = url.searchParams.get("userId");
      if (request.method === "POST") {
        const body = await request.json<{ userId?: unknown }>();
        userId = body.userId;
      }
      if (!isUserId(userId)) return json({ error: "invalid_subject" }, { status: 400 });
      const [metadata, membership] = await Promise.all([
        this.ctx.storage.get<OrganizationMetadata>("metadata"),
        this.ctx.storage.get<OrganizationMembership>(userMembershipStorageKey(userId)),
      ]);
      if (!isOrganizationMetadata(metadata)
        || !isOrganizationMembership(membership)
        || membership.userId !== userId
        || membership.organizationId !== metadata.id) {
        return json({ error: "not_found" }, { status: 404 });
      }
      const team = await this.ctx.storage.get<TeamRecord>(teamStorageKey(membership.teamId));
      if (!isTeamRecord(team)
        || team.id !== membership.teamId
        || team.organizationId !== metadata.id) {
        return json({ error: "not_found" }, { status: 404 });
      }
      return json({
        organizationId: metadata.id,
        teamId: membership.teamId,
        role: membership.role,
        authorizationEpoch: metadata.authorizationEpoch,
        capabilities: membership.role === "owner" ? OWNER_CAPABILITIES : membership.capabilities,
      } satisfies OrganizationGrant);
    }
    if (url.pathname === "/metadata") {
      const metadata = await this.ctx.storage.get<OrganizationMetadata>("metadata");
      if (!metadata || !isOrganizationMetadata(metadata)) {
        return json({ error: "not_found" }, { status: 404 });
      }
      if (request.method === "GET") return json(await organizationMetadataView(this.ctx.storage, metadata));
      if (request.method === "PATCH") {
        const body = await request.json<{ name?: unknown }>();
        if (Object.keys(body).length !== 1 || !("name" in body)) {
          return json({ error: "invalid_organization" }, { status: 400 });
        }
        const name = organizationName(body.name);
        if (name === undefined) return json({ error: "invalid_organization" }, { status: 400 });
        const updated = { ...metadata, name, updatedAt: Date.now() } satisfies OrganizationMetadata;
        await this.ctx.storage.put("metadata", updated);
        return json(await organizationMetadataView(this.ctx.storage, updated));
      }
      return methodNotAllowed();
    }
    return json({ error: "not_found" }, { status: 404 });
  }
}

export class ApiKeyRecord extends DurableObject<AccountAuthEnv> {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/resolve" && request.method === "GET") {
      const record = await this.ctx.storage.get<StoredApiKey>("record");
      return isStoredApiKey(record) ? json(record) : json({ error: "not_found" }, { status: 404 });
    }
    if (url.pathname === "/record" && request.method === "PUT") {
      if (await this.ctx.storage.get("record")) return json({ error: "conflict" }, { status: 409 });
      const input = await request.json<unknown>();
      if (!isApiKeyBase(input)) {
        return json({ error: "invalid_api_key" }, { status: 400 });
      }
      if (!isStoredApiKey(input)) return json({ error: "invalid_api_key" }, { status: 400 });
      const record = input;
      const account = await readAccount(this.env, record.userId);
      const grant = account ? await resolveOrganizationGrant(this.env, account) : undefined;
      if (!account
        || !grant
        || account.organizationId !== record.organizationId
        || grant.teamId !== record.teamId
        || grant.authorizationEpoch !== record.authorizationEpoch) {
        return json({ error: "invalid_api_key" }, { status: 400 });
      }
      await this.ctx.storage.put("record", record);
      return new Response(null, { status: 201 });
    }
    if (url.pathname === "/record" && request.method === "DELETE") {
      await this.ctx.storage.deleteAll();
      return new Response(null, { status: 204 });
    }
    return json({ error: "not_found" }, { status: 404 });
  }
}

function isUuid(value: unknown): value is string {
  return typeof value === "string" && UUID.test(value);
}

async function organizationMetadataView(
  storage: DurableObjectStorage,
  metadata: OrganizationMetadata,
): Promise<Readonly<{
  id: string;
  name: string | null;
  rootTeam: Readonly<{ id: string; name: string | null }>;
  authorizationEpoch: number;
  createdAt: number;
  updatedAt: number;
}>> {
  const rootTeam = await storage.get<TeamRecord>(teamStorageKey(metadata.rootTeamId));
  if (!isTeamRecord(rootTeam)
    || rootTeam.id !== metadata.rootTeamId
    || rootTeam.organizationId !== metadata.id) {
    throw new Error("organization root team is unavailable");
  }
  return {
    id: metadata.id,
    name: metadata.name,
    rootTeam: { id: rootTeam.id, name: rootTeam.name },
    authorizationEpoch: metadata.authorizationEpoch,
    createdAt: metadata.createdAt,
    updatedAt: metadata.updatedAt,
  };
}

function isUserRecord(value: unknown): value is UserRecord {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Partial<UserRecord>;
  return isUserId(record.id)
    && isUuid(record.organizationId)
    && typeof record.persistent === "boolean"
    && Number.isFinite(record.createdAt)
    && Number.isFinite(record.lastAuthenticatedAt);
}

function isOrganizationMetadata(value: unknown): value is OrganizationMetadata {
  if (typeof value !== "object" || value === null) return false;
  const metadata = value as Partial<OrganizationMetadata>;
  return isUuid(metadata.id)
    && (metadata.name === null || typeof metadata.name === "string")
    && isUuid(metadata.rootTeamId)
    && Number.isSafeInteger(metadata.authorizationEpoch)
    && Number(metadata.authorizationEpoch) >= 1
    && Number.isFinite(metadata.createdAt)
    && Number.isFinite(metadata.updatedAt);
}

function isOrganizationMembership(value: unknown): value is OrganizationMembership {
  if (typeof value !== "object" || value === null) return false;
  const membership = value as Partial<OrganizationMembership>;
  return isUserId(membership.userId)
    && isUuid(membership.organizationId)
    && isUuid(membership.teamId)
    && isOrganizationRole(membership.role)
    && isOrganizationCapabilities(membership.capabilities)
    && Number.isFinite(membership.createdAt);
}

function isTeamRecord(value: unknown): value is TeamRecord {
  if (typeof value !== "object" || value === null) return false;
  const team = value as Partial<TeamRecord>;
  return isUuid(team.id)
    && isUuid(team.organizationId)
    && (team.parentTeamId === null || isUuid(team.parentTeamId))
    && (team.name === null || typeof team.name === "string")
    && Number.isFinite(team.createdAt)
    && Number.isFinite(team.updatedAt);
}

function isOrganizationRole(value: unknown): value is OrganizationRole {
  return value === "owner" || value === "writer" || value === "reader";
}

function organizationRoleRank(role: OrganizationRole): number {
  return role === "owner" ? 2 : role === "writer" ? 1 : 0;
}

export function isOrganizationCapabilities(value: unknown): value is readonly OrganizationCapability[] {
  if (!Array.isArray(value) || new Set(value).size !== value.length) return false;
  return value.every((capability) =>
    capability === "agents:read"
    || capability === "agents:portability"
    || capability === "agents:write"
    || capability === "api_keys:read"
    || capability === "api_keys:write"
    || capability === "history:read"
    || capability === "memory:read"
    || capability === "memory:write"
    || capability === "tools:use"
    || capability === "organization:read"
    || capability === "organization:write"
  );
}

const CONNECT_CAPABILITIES = new Set<OrganizationCapability>([
  "agents:read",
  "agents:portability",
  "agents:write",
  "history:read",
  "memory:read",
  "memory:write",
  "tools:use",
]);
const CONNECT_CONNECTORS = new Set<ConnectConnectorId>([
  ...CONNECTOR_CAPABILITY_IDS,
  "chatgpt",
]);
const CONNECT_GRANT_ID = /^0x[0-9a-fA-F]{64}$/;
const CONNECT_MCP_ID = /^[A-Za-z0-9_-]{43}$/;

function parseConnectGrantAssertions(headers: Headers): Readonly<{
  capabilities: readonly OrganizationCapability[];
  slice: ConnectGrantSlice;
}> | undefined {
  const grantId = headers.get(CONNECT_GRANT_ID_HEADER);
  const capabilities = parseUniqueJsonArray(headers.get(CONNECT_CAPABILITIES_HEADER));
  const connectors = parseUniqueJsonArray(headers.get(CONNECT_CONNECTORS_HEADER));
  const encodedConnectorConnections = headers.get(CONNECT_CONNECTOR_CONNECTIONS_HEADER);
  const connectorConnections = encodedConnectorConnections === null
    ? undefined
    : parseConnectorConnectionSelection(encodedConnectorConnections);
  const mcpIds = parseUniqueJsonArray(headers.get(CONNECT_MCP_IDS_HEADER));
  const appToolCatalogDigest = headers.get(CONNECT_APP_TOOL_CATALOG_DIGEST_HEADER);
  if (!grantId || !CONNECT_GRANT_ID.test(grantId)
    || !capabilities || !capabilities.every((value): value is OrganizationCapability => (
      CONNECT_CAPABILITIES.has(value as OrganizationCapability)
    ))
    || !connectors || !connectors.every((value): value is ConnectConnectorId => (
      CONNECT_CONNECTORS.has(value as ConnectConnectorId)
    ))
    || (encodedConnectorConnections !== null && connectorConnections === undefined)
    || (connectorConnections !== undefined
      && Object.keys(connectorConnections).some((capability) => (
        !connectors.includes(capability as ConnectConnectorId)
      )))
    || !mcpIds || mcpIds.length > 16 || !mcpIds.every((value) => CONNECT_MCP_ID.test(value))
    || (appToolCatalogDigest !== null && !isAppToolCatalogDigest(appToolCatalogDigest))) {
    return undefined;
  }
  return {
    capabilities,
    slice: {
      grantId: grantId.toLowerCase(),
      connectors,
      ...(connectorConnections === undefined ? {} : { connectorConnections }),
      mcpIds,
      ...(appToolCatalogDigest === null ? {} : {
        appToolCatalogDigest: appToolCatalogDigest as `0x${string}`,
      }),
    },
  };
}

function parseConnectorConnectionSelection(
  encoded: string,
): ConnectorConnectionSelection | undefined {
  let value: unknown;
  try { value = JSON.parse(encoded); } catch { return undefined; }
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const result: Partial<Record<ConnectorCapabilityId, readonly string[]>> = {};
  for (const [key, ids] of Object.entries(value)) {
    const capability = CONNECTOR_CAPABILITY_IDS.find((candidate) => candidate === key);
    if (!capability || !Array.isArray(ids) || ids.length > 64
      || ids.some((id) => typeof id !== "string" || !CONNECT_MCP_ID.test(id))
      || new Set(ids).size !== ids.length) return undefined;
    result[capability] = ids as string[];
  }
  return result;
}

function parseUniqueJsonArray(encoded: string | null): string[] | undefined {
  if (encoded === null) return undefined;
  let value: unknown;
  try { value = JSON.parse(encoded); } catch { return undefined; }
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")
    || new Set(value).size !== value.length) return undefined;
  return value as string[];
}

function isOrganizationGrant(value: unknown): value is OrganizationGrant {
  if (typeof value !== "object" || value === null) return false;
  const grant = value as Partial<OrganizationGrant>;
  return isUuid(grant.organizationId)
    && isUuid(grant.teamId)
    && isOrganizationRole(grant.role)
    && Number.isSafeInteger(grant.authorizationEpoch)
    && Number(grant.authorizationEpoch) >= 1
    && isOrganizationCapabilities(grant.capabilities);
}

function isApiKeyBase(value: unknown): value is ApiKeyBase {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Partial<ApiKeyBase>;
  return typeof record.id === "string"
    && /^[A-Za-z0-9_-]{12}$/.test(record.id)
    && typeof record.label === "string"
    && record.label.length <= 120
    && record.prefix === `ncx_live_${record.id}`
    && Number.isFinite(record.createdAt)
    && typeof record.digest === "string"
    && /^[A-Za-z0-9_-]{43}$/.test(record.digest)
    && isUserId(record.userId);
}

function isStoredApiKey(value: unknown): value is StoredApiKey {
  if (!isApiKeyBase(value)) return false;
  const record = value as Partial<StoredApiKey>;
  return isUuid(record.organizationId)
    && isUuid(record.teamId)
    && isOrganizationRole(record.role)
    && isOrganizationCapabilities(record.capabilities)
    && Number.isSafeInteger(record.authorizationEpoch)
    && Number(record.authorizationEpoch) >= 1;
}

function sameStoredApiKey(value: unknown, expected: StoredApiKey): boolean {
  if (!isStoredApiKey(value)) return false;
  return value.id === expected.id
    && value.label === expected.label
    && value.prefix === expected.prefix
    && value.createdAt === expected.createdAt
    && value.digest === expected.digest
    && value.userId === expected.userId
    && value.organizationId === expected.organizationId
    && value.teamId === expected.teamId
    && value.role === expected.role
    && value.authorizationEpoch === expected.authorizationEpoch
    && value.capabilities.length === expected.capabilities.length
    && value.capabilities.every((capability, index) => capability === expected.capabilities[index]);
}

function organizationName(value: unknown): string | null | undefined {
  if (value === null) return null;
  if (typeof value !== "string") return undefined;
  const name = value.trim();
  return name.length <= 120 ? name : undefined;
}

function proxyOrganizationResponse(response: Response): Response {
  return response;
}

function randomBase64Url(bytes: number): string {
  const value = crypto.getRandomValues(new Uint8Array(bytes));
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function sha256(value: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  let binary = "";
  for (const byte of digest) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function readJson(request: Request): Promise<Record<string, unknown> | Response> {
  if (!request.headers.get("content-type")?.toLowerCase().startsWith("application/json")) {
    return json({ error: "expected_json" }, { status: 415 });
  }
  try {
    const value = await request.json<unknown>();
    if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error();
    return value as Record<string, unknown>;
  } catch {
    return json({ error: "invalid_json" }, { status: 400 });
  }
}

function json(body: unknown, init: ResponseInit = {}): Response {
  const headers = new Headers(init.headers);
  headers.set("cache-control", "no-store");
  headers.set("x-content-type-options", "nosniff");
  return Response.json(body, {
    ...init,
    headers,
  });
}

function unauthorized(): Response {
  return json({ error: "unauthorized" }, { status: 401 });
}

function methodNotAllowed(): Response {
  return json({ error: "method_not_allowed" }, { status: 405 });
}
