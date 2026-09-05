import { DurableObject } from "cloudflare:workers";

import {
  CredentialVault,
  type CredentialVaultEnv,
  type EncryptedEnvelope,
} from "./credential-vault";
import { canonicalRemoteMcpTarget } from "../../mcp-target.mjs";

const STATE_KEY = "mcp-connection-state";
const CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const PENDING_TTL_MS = 10 * 60_000;
const EXPIRY_SKEW_MS = 30_000;
const MAX_CONTROL_BODY_BYTES = 16 * 1024;
const MAX_PROVIDER_BODY_BYTES = 64 * 1024;
const MAX_REPLAY_BODY_BYTES = 8 * 1024 * 1024;
const MAX_REPLAY_CAPTURE_MS = 60_000;
const MAX_TOKEN_BYTES = 16 * 1024;
const MAX_URL_BYTES = 2_048;
const MAX_AUTHORIZATION_URL_BYTES = 8 * 1024;
const OAUTH_TIMEOUT_MS = 20_000;
const MCP_TIMEOUT_MS = 60_000;
const REDIRECT_STATUS = new Set([301, 302, 303, 307, 308]);
const MCP_REQUEST_HEADERS = [
  "accept",
  "content-type",
  "mcp-protocol-version",
  "mcp-session-id",
  "last-event-id",
] as const;
const MCP_RESPONSE_HEADERS = ["content-type", "mcp-session-id", "retry-after"] as const;

export interface McpConnectionBrokerEnv extends CredentialVaultEnv {
  MCP_OAUTH_CLIENT_ID?: string;
  MCP_OAUTH_CLIENT_SECRET?: string;
}

type PublicStatus =
  | "authorization_required"
  | "connected"
  | "reauthorization_required"
  | "disabled"
  | "revoked";

type OAuthClient = {
  id: string;
  secret?: string;
  tokenEndpointAuthMethod: "none" | "client_secret_post";
};

type OAuthMetadata = {
  resource: string;
  authorizationEndpoint: string;
  tokenEndpoint: string;
  registrationEndpoint?: string;
  revocationEndpoint?: string;
  scopesSupported: string[];
};

type PendingAuthorization = {
  state: string;
  verifier: string;
  redirectUri: string;
  returnTo: string;
  expiresAt: number;
  scopes: string[];
  client: OAuthClient;
  metadata: OAuthMetadata;
};

type StoredAuthorization = {
  accessToken: string;
  refreshToken?: string;
  expiresAt?: number;
  scopes: string[];
  client: OAuthClient;
  metadata: OAuthMetadata;
};

type StoredConnection = {
  id: string;
  endpoint: string;
  name: string;
  requestedScopes: string[];
  lifecycle: "active" | "disabled" | "revoked";
  createdAt: number;
  revokedAt?: number;
  reauthorizationRequired?: boolean;
  pending?: PendingAuthorization;
  authorization?: StoredAuthorization;
};

type ConnectionState = {
  version: 1;
  connections: Record<string, StoredConnection>;
};

type StoredRow = { envelope: EncryptedEnvelope };

type ProtectedResourceMetadata = {
  resource: string;
  authorizationServers: string[];
  scopesSupported: string[];
};

type AuthorizationServerMetadata = {
  authorizationEndpoint: string;
  tokenEndpoint: string;
  registrationEndpoint?: string;
  revocationEndpoint?: string;
  scopesSupported: string[];
};

/** Global opaque connection-ID ownership, isolated from agent subject credentials. */
export class McpConnectionDirectory extends DurableObject<Record<string, never>> {
  readonly #state: DurableObjectState;
  readonly #id: string | undefined;

  constructor(state: DurableObjectState, env: Record<string, never>) {
    super(state, env);
    this.#state = state;
    this.#id = state.id.name;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const body = await readJson(request, 2_048);
    const id = stringField(body, "id");
    if (request.method !== "POST" || !id || id !== this.#id || !CONNECTION_ID.test(id)) {
      return jsonError(400, "invalid_request");
    }
    if (url.pathname === "/v1/bind") {
      const userId = stringField(body, "user_id");
      if (!userId || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(userId)) {
        return jsonError(400, "invalid_request");
      }
      const result = await this.#state.storage.transaction(async (transaction) => {
        const current = await transaction.get<string>("owner");
        if (current && current !== userId) return "conflict" as const;
        if (!current) await transaction.put("owner", userId);
        return current ? "unchanged" as const : "bound" as const;
      });
      return result === "conflict"
        ? jsonError(409, "mcp_connection_owner_mismatch")
        : json({ status: result }, 200);
    }
    if (url.pathname === "/v1/resolve") {
      const owner = await this.#state.storage.get<string>("owner");
      return owner ? json({ user_id: owner }, 200) : jsonError(404, "mcp_connection_not_found");
    }
    return jsonError(404, "not_found");
  }
}

/** Generic OAuth-protected remote MCP connections owned by one user broker. */
export class McpConnectionOwner {
  readonly #storage: DurableObjectStorage;
  readonly #env: McpConnectionBrokerEnv;
  readonly #vault: CredentialVault;
  #state: ConnectionState = { version: 1, connections: {} };
  #initializationFailed = false;

  constructor(
    state: DurableObjectState,
    env: McpConnectionBrokerEnv,
  ) {
    this.#storage = state.storage;
    this.#env = env;
    this.#vault = new CredentialVault(env, `mcp-connections/${state.id.toString()}`);
  }

  async initialize(): Promise<void> {
    try {
      const row = await this.#storage.get<StoredRow>(STATE_KEY);
      if (!row) return;
      const opened = await this.#vault.open<ConnectionState>(row.envelope);
      this.#state = opened.value;
      if (opened.reseal) await this.#persist();
    } catch {
      this.#initializationFailed = true;
    }
  }

  async fetch(request: Request): Promise<Response> {
    if (this.#initializationFailed) return jsonError(503, "mcp_connection_broker_unavailable");
    try {
      return await this.#dispatch(request);
    } catch (error) {
      const problem = mcpFailure(error);
      await this.#restore();
      return jsonError(problem.status, problem.code);
    }
  }

  async #dispatch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.origin !== "https://mcp-connections.internal" || url.search || url.hash) {
      return jsonError(403, "destination_denied");
    }
    if (request.method === "GET" && url.pathname === "/v1/connections") {
      return json(this.#publicWire(), 200);
    }
    const match = url.pathname.match(
      /^\/v1\/connections\/([A-Za-z0-9_-]{43})(?:\/(start|callback|proxy))?$/,
    );
    const id = match?.[1];
    const operation = match?.[2];
    if (!id || !CONNECTION_ID.test(id)) return jsonError(404, "not_found");
    if (operation === "proxy") return this.#proxy(id, request);
    if (request.method === "PUT" && operation === undefined) {
      return json(this.#publicWire([await this.#materialize(id, request)]), 200);
    }
    if (request.method === "GET" && operation === undefined) {
      return json(this.#publicWire([this.#connection(id)]), 200);
    }
    if (request.method === "POST" && operation === "start") {
      const authorizationUrl = await this.#start(id, request);
      return json({
        ...this.#publicWire([this.#connection(id)]),
        authorization_url: authorizationUrl,
      }, 200);
    }
    if (request.method === "POST" && operation === "callback") {
      const callback = await this.#callback(id, request);
      return json({
        ...this.#publicWire([this.#connection(id)]),
        return_to: callback.returnTo,
      }, 200);
    }
    if (request.method === "DELETE" && operation === undefined) {
      await this.#revoke(id);
      return json(this.#publicWire([this.#connection(id)]), 200);
    }
    return jsonError(405, "method_not_allowed");
  }

  async #materialize(id: string, request: Request): Promise<StoredConnection> {
    const body = await readJson(request, MAX_CONTROL_BODY_BYTES);
    const materialization = mcpMaterialization(body);
    if (!materialization) throw new McpFailure(400, "invalid_request");
    const { endpoint, name, requestedScopes } = materialization;
    const existing = this.#state.connections[id];
    if (existing) {
      if (existing.endpoint !== endpoint) {
        throw new McpFailure(409, "mcp_connection_substitution_denied");
      }
      return existing;
    }
    const connection: StoredConnection = {
      id,
      endpoint,
      name,
      requestedScopes,
      lifecycle: "active",
      createdAt: Date.now(),
    };
    this.#state.connections[id] = connection;
    await this.#persist();
    return connection;
  }

  async #start(id: string, request: Request): Promise<string> {
    const connection = this.#connection(id);
    this.#requireActive(connection);
    const body = await readJson(request, MAX_CONTROL_BODY_BYTES);
    const redirectUri = stringField(body, "redirect_uri");
    const returnTo = stringField(body, "return_to");
    if (!redirectUri || !validRedirectUri(redirectUri, this.#env)
      || !returnTo || !validReturnTo(returnTo)) {
      throw new McpFailure(400, "invalid_request");
    }
    const metadata = await discoverOAuth(connection.endpoint);
    const scopes = selectedScopes(connection, metadata.scopesSupported);
    const client = await oauthClient(this.#env, metadata, redirectUri);
    const verifier = randomBase64Url(64);
    const state = randomBase64Url(32);
    const challenge = encodeBase64Url(new Uint8Array(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)),
    ));
    connection.pending = {
      state,
      verifier,
      redirectUri,
      returnTo,
      expiresAt: Date.now() + PENDING_TTL_MS,
      scopes,
      client,
      metadata,
    };
    const authorization = new URL(metadata.authorizationEndpoint);
    authorization.searchParams.set("response_type", "code");
    authorization.searchParams.set("client_id", client.id);
    authorization.searchParams.set("redirect_uri", redirectUri);
    authorization.searchParams.set("state", state);
    authorization.searchParams.set("code_challenge", challenge);
    authorization.searchParams.set("code_challenge_method", "S256");
    authorization.searchParams.set("resource", metadata.resource);
    if (scopes.length > 0) authorization.searchParams.set("scope", scopes.join(" "));
    if (authorization.href.length > MAX_AUTHORIZATION_URL_BYTES) {
      throw new McpFailure(502, "mcp_authorization_url_too_large");
    }
    await this.#persist();
    return authorization.href;
  }

  async #callback(id: string, request: Request): Promise<{ returnTo: string }> {
    const connection = this.#connection(id);
    this.#requireActive(connection);
    const body = await readJson(request, MAX_CONTROL_BODY_BYTES);
    const code = stringField(body, "code");
    const state = stringField(body, "state");
    const pending = connection.pending;
    if (!pending || pending.expiresAt <= Date.now() || !state || state !== pending.state) {
      throw new McpFailure(400, "invalid_oauth_state");
    }
    delete connection.pending;
    await this.#persist();
    if (!code) return { returnTo: pending.returnTo };
    const parameters = new URLSearchParams({
      grant_type: "authorization_code",
      code,
      redirect_uri: pending.redirectUri,
      code_verifier: pending.verifier,
      client_id: pending.client.id,
      resource: pending.metadata.resource,
    });
    addClientSecret(parameters, pending.client);
    const response = await oauthFetch(new Request(pending.metadata.tokenEndpoint, {
      method: "POST",
      headers: oauthFormHeaders(),
      body: parameters,
    }));
    if (!response.ok || REDIRECT_STATUS.has(response.status)) {
      await cancelBody(response);
      throw new McpFailure(502, "mcp_token_exchange_failed");
    }
    const token = decodeToken(await responseJson(response), pending.scopes);
    rejectScopeEscalation(token.scopes, pending.scopes);
    connection.authorization = {
      accessToken: token.accessToken,
      ...(token.refreshToken ? { refreshToken: token.refreshToken } : {}),
      ...(token.expiresIn === undefined
        ? {}
        : { expiresAt: Date.now() + token.expiresIn * 1_000 }),
      scopes: token.scopes,
      client: pending.client,
      metadata: pending.metadata,
    };
    delete connection.reauthorizationRequired;
    await this.#persist();
    return { returnTo: pending.returnTo };
  }

  async #proxy(id: string, request: Request): Promise<Response> {
    const connection = this.#connection(id);
    this.#requireActive(connection);
    if (request.headers.has("authorization") || request.headers.has("cookie")
      || request.headers.has("proxy-authorization")) {
      throw new McpFailure(403, "caller_credential_forbidden");
    }
    let usable = await this.#usableAuthorization(connection);
    let authorization = usable.authorization;
    const captured = captureReplayBody(request.body);
    const firstRequest = mcpUpstreamRequest(
      connection.endpoint,
      request,
      authorization.accessToken,
      captured.stream,
    );
    const [first, replayBody] = await Promise.all([
      mcpFetch(firstRequest),
      captured.replay,
    ]);
    let upstream = first;
    if (upstream.status === 401) {
      await cancelBody(upstream);
      if (usable.refreshed || !authorization.refreshToken) {
        return this.#reauthorizationRequired(connection);
      }
      authorization = await this.#refresh(connection, authorization);
      usable = {
        authorization,
        refreshed: true,
        projectionCredentials: unique([
          ...usable.projectionCredentials,
          ...authorizationCredentials(authorization),
        ]),
      };
      if (request.body && replayBody === undefined) {
        throw new McpFailure(413, "mcp_request_not_replayable");
      }
      upstream = await mcpFetch(mcpUpstreamRequest(
        connection.endpoint,
        request,
        authorization.accessToken,
        replayBody ?? null,
      ));
      if (upstream.status === 401) {
        await cancelBody(upstream);
        return this.#reauthorizationRequired(connection);
      }
    }
    if (REDIRECT_STATUS.has(upstream.status)) {
      await cancelBody(upstream);
      throw new McpFailure(502, "mcp_redirect_blocked");
    }
    return safeMcpResponse(upstream, usable.projectionCredentials);
  }

  async #usableAuthorization(connection: StoredConnection): Promise<{
    authorization: StoredAuthorization;
    refreshed: boolean;
    projectionCredentials: string[];
  }> {
    if (connection.reauthorizationRequired || !connection.authorization) {
      throw new McpFailure(409, connection.authorization
        ? "reauthorization_required"
        : "authorization_required");
    }
    if (connection.authorization.expiresAt === undefined
      || connection.authorization.expiresAt > Date.now() + EXPIRY_SKEW_MS) {
      return {
        authorization: connection.authorization,
        refreshed: false,
        projectionCredentials: authorizationCredentials(connection.authorization),
      };
    }
    if (!connection.authorization.refreshToken) {
      return this.#reauthorizationRequired(connection);
    }
    const previous = connection.authorization;
    const refreshed = await this.#refresh(connection, previous);
    return {
      authorization: refreshed,
      refreshed: true,
      projectionCredentials: unique([
        ...authorizationCredentials(previous),
        ...authorizationCredentials(refreshed),
      ]),
    };
  }

  async #refresh(
    connection: StoredConnection,
    current: StoredAuthorization,
  ): Promise<StoredAuthorization> {
    const parameters = new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: current.refreshToken!,
      client_id: current.client.id,
      resource: current.metadata.resource,
    });
    if (current.scopes.length > 0) parameters.set("scope", current.scopes.join(" "));
    addClientSecret(parameters, current.client);
    const response = await oauthFetch(new Request(current.metadata.tokenEndpoint, {
      method: "POST",
      headers: oauthFormHeaders(),
      body: parameters,
    }));
    if (!response.ok || REDIRECT_STATUS.has(response.status)) {
      const rejected = response.status === 400 || response.status === 401;
      await cancelBody(response);
      if (rejected) return this.#reauthorizationRequired(connection);
      throw new McpFailure(503, "mcp_oauth_provider_unavailable");
    }
    let token;
    try {
      token = decodeToken(await responseJson(response), current.scopes);
      rejectScopeEscalation(token.scopes, current.scopes);
    } catch {
      return this.#reauthorizationRequired(connection);
    }
    const {
      expiresAt: _previousExpiry,
      refreshToken: previousRefreshToken,
      ...retained
    } = current;
    const refreshed: StoredAuthorization = {
      ...retained,
      accessToken: token.accessToken,
      ...(token.refreshToken ?? previousRefreshToken
        ? { refreshToken: (token.refreshToken ?? previousRefreshToken)! }
        : {}),
      ...(token.expiresIn === undefined
        ? {}
        : { expiresAt: Date.now() + token.expiresIn * 1_000 }),
      scopes: token.scopes,
    };
    connection.authorization = refreshed;
    await this.#persist();
    return refreshed;
  }

  async #reauthorizationRequired(connection: StoredConnection): Promise<never> {
    connection.reauthorizationRequired = true;
    delete connection.authorization;
    await this.#persist();
    throw new McpFailure(409, "reauthorization_required");
  }

  async #revoke(id: string): Promise<void> {
    const connection = this.#connection(id);
    if (connection.lifecycle === "revoked") return;
    const authorization = connection.authorization;
    if (authorization?.metadata.revocationEndpoint) {
      const token = authorization.refreshToken ?? authorization.accessToken;
      const parameters = new URLSearchParams({
        token,
        token_type_hint: authorization.refreshToken ? "refresh_token" : "access_token",
        client_id: authorization.client.id,
      });
      addClientSecret(parameters, authorization.client);
      try {
        const response = await oauthFetch(new Request(authorization.metadata.revocationEndpoint, {
          method: "POST",
          headers: oauthFormHeaders(),
          body: parameters,
        }));
        await cancelBody(response);
      } catch {
        // The local tombstone is the authoritative ability-to-use boundary.
      }
    }
    connection.lifecycle = "revoked";
    connection.revokedAt = Date.now();
    delete connection.pending;
    delete connection.authorization;
    delete connection.reauthorizationRequired;
    await this.#persist();
  }

  #connection(id: string): StoredConnection {
    const connection = this.#state.connections[id];
    if (!connection) throw new McpFailure(404, "mcp_connection_not_found");
    return connection;
  }

  #requireActive(connection: StoredConnection): void {
    if (connection.lifecycle === "revoked") throw new McpFailure(409, "connection_revoked");
    if (connection.lifecycle === "disabled") throw new McpFailure(409, "connection_disabled");
  }

  #publicWire(connections = Object.values(this.#state.connections)): {
    mcp_connections: Array<{ id: string; name: string; status: PublicStatus }>;
  } {
    return {
      mcp_connections: connections
        .map((connection) => ({
          id: connection.id,
          name: connection.name,
          status: publicStatus(connection),
        }))
        .sort((left, right) => left.id.localeCompare(right.id)),
    };
  }

  async #persist(): Promise<void> {
    await this.#storage.put(STATE_KEY, {
      envelope: await this.#vault.seal(this.#state),
    } satisfies StoredRow);
  }

  async #restore(): Promise<void> {
    try {
      const row = await this.#storage.get<StoredRow>(STATE_KEY);
      this.#state = row
        ? (await this.#vault.open<ConnectionState>(row.envelope)).value
        : { version: 1, connections: {} };
    } catch {
      // Preserve the in-memory state rather than silently deleting encrypted state.
    }
  }
}

function publicStatus(connection: StoredConnection): PublicStatus {
  if (connection.lifecycle === "revoked") return "revoked";
  if (connection.lifecycle === "disabled") return "disabled";
  if (connection.reauthorizationRequired) return "reauthorization_required";
  if (connection.authorization?.expiresAt !== undefined
    && connection.authorization.expiresAt <= Date.now() + EXPIRY_SKEW_MS
    && !connection.authorization.refreshToken) return "reauthorization_required";
  return connection.authorization ? "connected" : "authorization_required";
}

async function discoverOAuth(endpointValue: string): Promise<OAuthMetadata> {
  const endpoint = new URL(endpointValue);
  const challenge = await oauthFetch(new Request(endpoint, {
    method: "GET",
    headers: { accept: "application/json, text/event-stream" },
  }));
  const advertised = challenge.status === 401
    ? resourceMetadataFromChallenge(challenge.headers.get("www-authenticate"))
    : undefined;
  await cancelBody(challenge);
  const candidates = unique([
    advertised,
    wellKnownUrl("oauth-protected-resource", endpoint).href,
    new URL("/.well-known/oauth-protected-resource", endpoint.origin).href,
  ].filter((value): value is string => Boolean(value)));
  let protectedMetadata: ProtectedResourceMetadata | undefined;
  for (const candidate of candidates) {
    const response = await oauthFetch(new Request(candidate, { headers: { accept: "application/json" } }));
    if (!response.ok || REDIRECT_STATUS.has(response.status)) {
      await cancelBody(response);
      continue;
    }
    protectedMetadata = decodeProtectedResourceMetadata(await responseJson(response), endpoint);
    break;
  }
  if (!protectedMetadata) throw new McpFailure(502, "mcp_oauth_discovery_failed");
  const issuer = new URL(protectedMetadata.authorizationServers[0]!);
  const serverResponse = await oauthFetch(new Request(
    wellKnownUrl("oauth-authorization-server", issuer),
    { headers: { accept: "application/json" } },
  ));
  if (!serverResponse.ok || REDIRECT_STATUS.has(serverResponse.status)) {
    await cancelBody(serverResponse);
    throw new McpFailure(502, "mcp_oauth_discovery_failed");
  }
  const server = decodeAuthorizationServerMetadata(await responseJson(serverResponse), issuer);
  return {
    resource: protectedMetadata.resource,
    authorizationEndpoint: server.authorizationEndpoint,
    tokenEndpoint: server.tokenEndpoint,
    ...(server.registrationEndpoint ? { registrationEndpoint: server.registrationEndpoint } : {}),
    ...(server.revocationEndpoint ? { revocationEndpoint: server.revocationEndpoint } : {}),
    scopesSupported: unique([
      ...protectedMetadata.scopesSupported,
      ...server.scopesSupported,
    ]),
  };
}

function resourceMetadataFromChallenge(value: string | null): string | undefined {
  if (!value) return undefined;
  const match = value.match(/(?:^|[,\s])resource_metadata\s*=\s*"([^"]+)"/i);
  if (!match) return undefined;
  const url = safePublicHttpsUrl(match[1]!);
  if (!url) throw new McpFailure(502, "unsafe_mcp_oauth_metadata");
  return url.href;
}

function decodeProtectedResourceMetadata(
  value: unknown,
  endpoint: URL,
): ProtectedResourceMetadata {
  if (!isRecord(value)) throw new McpFailure(502, "invalid_mcp_oauth_metadata");
  const resource = typeof value.resource === "string" ? safePublicHttpsUrl(value.resource) : endpoint;
  if (!resource || resource.href !== endpoint.href) {
    throw new McpFailure(502, "unsafe_mcp_oauth_metadata");
  }
  const authorizationServers = safeUrlArray(value.authorization_servers);
  if (!authorizationServers || authorizationServers.length === 0) {
    throw new McpFailure(502, "invalid_mcp_oauth_metadata");
  }
  return {
    resource: resource.href,
    authorizationServers,
    scopesSupported: metadataStringArray(value.scopes_supported),
  };
}

function decodeAuthorizationServerMetadata(
  value: unknown,
  issuer: URL,
): AuthorizationServerMetadata {
  if (!isRecord(value)) throw new McpFailure(502, "invalid_mcp_oauth_metadata");
  if (typeof value.issuer === "string") {
    const advertisedIssuer = safePublicHttpsUrl(value.issuer);
    if (!advertisedIssuer || advertisedIssuer.href !== issuer.href) {
      throw new McpFailure(502, "unsafe_mcp_oauth_metadata");
    }
  }
  const authorizationEndpoint = metadataUrl(value.authorization_endpoint);
  const tokenEndpoint = metadataUrl(value.token_endpoint);
  if (!authorizationEndpoint || !tokenEndpoint) {
    throw new McpFailure(502, "invalid_mcp_oauth_metadata");
  }
  if (Array.isArray(value.code_challenge_methods_supported)
    && !value.code_challenge_methods_supported.includes("S256")) {
    throw new McpFailure(502, "mcp_pkce_s256_unsupported");
  }
  const registrationEndpoint = optionalMetadataUrl(value.registration_endpoint);
  const revocationEndpoint = optionalMetadataUrl(value.revocation_endpoint);
  return {
    authorizationEndpoint,
    tokenEndpoint,
    ...(registrationEndpoint ? { registrationEndpoint } : {}),
    ...(revocationEndpoint ? { revocationEndpoint } : {}),
    scopesSupported: metadataStringArray(value.scopes_supported),
  };
}

async function oauthClient(
  env: McpConnectionBrokerEnv,
  metadata: OAuthMetadata,
  redirectUri: string,
): Promise<OAuthClient> {
  if (metadata.registrationEndpoint) {
    const response = await oauthFetch(new Request(metadata.registrationEndpoint, {
      method: "POST",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: JSON.stringify({
        client_name: "Nanocodex",
        redirect_uris: [redirectUri],
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
      }),
    }));
    if (!response.ok || REDIRECT_STATUS.has(response.status)) {
      await cancelBody(response);
      throw new McpFailure(502, "mcp_dynamic_registration_failed");
    }
    const registration = await responseJson(response);
    const id = stringField(registration, "client_id");
    const secret = stringField(registration, "client_secret");
    const method = stringField(registration, "token_endpoint_auth_method") ?? (secret
      ? "client_secret_post"
      : "none");
    if (!id || id.length > 2_048 || (secret && secret.length > MAX_TOKEN_BYTES)
      || (method !== "none" && method !== "client_secret_post")) {
      throw new McpFailure(502, "invalid_mcp_registration_response");
    }
    return {
      id,
      ...(secret ? { secret } : {}),
      tokenEndpointAuthMethod: method,
    };
  }
  const id = env.MCP_OAUTH_CLIENT_ID?.trim();
  const secret = env.MCP_OAUTH_CLIENT_SECRET?.trim();
  if (!id || id.length > 2_048 || (secret && secret.length > MAX_TOKEN_BYTES)) {
    throw new McpFailure(503, "mcp_oauth_client_unavailable");
  }
  if (looksLikeUrl(id) && !safePublicHttpsUrl(id)) {
    throw new McpFailure(503, "unsafe_mcp_oauth_client");
  }
  return {
    id,
    ...(secret ? { secret } : {}),
    tokenEndpointAuthMethod: secret ? "client_secret_post" : "none",
  };
}

function selectedScopes(connection: StoredConnection, supported: string[]): string[] {
  if (connection.requestedScopes.length > 0) {
    if (supported.length > 0
      && connection.requestedScopes.some((scope) => !supported.includes(scope))) {
      throw new McpFailure(400, "mcp_scope_unsupported");
    }
    return [...connection.requestedScopes];
  }
  return new URL(connection.endpoint).hostname === "mcp.linear.app" && supported.includes("read")
    ? ["read"]
    : [];
}

function decodeToken(value: unknown, fallbackScopes: string[]): {
  accessToken: string;
  refreshToken?: string;
  expiresIn?: number;
  scopes: string[];
} {
  if (!isRecord(value) || typeof value.access_token !== "string" || !validToken(value.access_token)
    || (value.token_type !== undefined
      && (typeof value.token_type !== "string" || value.token_type.toLowerCase() !== "bearer"))) {
    throw new McpFailure(502, "invalid_mcp_token_response");
  }
  const refreshToken = typeof value.refresh_token === "string" ? value.refresh_token : undefined;
  if (refreshToken !== undefined && !validToken(refreshToken)) {
    throw new McpFailure(502, "invalid_mcp_token_response");
  }
  const expiresIn = value.expires_in === undefined ? undefined : value.expires_in;
  if (expiresIn !== undefined && (typeof expiresIn !== "number"
    || !Number.isSafeInteger(expiresIn) || expiresIn <= 0 || expiresIn > 31_536_000)) {
    throw new McpFailure(502, "invalid_mcp_token_response");
  }
  const scopes = typeof value.scope === "string" && value.scope.trim()
    ? value.scope.trim().split(/\s+/)
    : [...fallbackScopes];
  if (!validScopes(scopes)) throw new McpFailure(502, "invalid_mcp_token_response");
  return {
    accessToken: value.access_token,
    ...(refreshToken ? { refreshToken } : {}),
    ...(typeof expiresIn === "number" ? { expiresIn } : {}),
    scopes,
  };
}

function rejectScopeEscalation(granted: string[], requested: string[]): void {
  if (requested.length > 0 && granted.some((scope) => !requested.includes(scope))) {
    throw new McpFailure(502, "mcp_scope_escalation_blocked");
  }
}

function mcpUpstreamRequest(
  endpoint: string,
  original: Request,
  accessToken: string,
  body: BodyInit | null,
): Request {
  const headers = new Headers({ authorization: `Bearer ${accessToken}` });
  for (const name of MCP_REQUEST_HEADERS) {
    const value = boundedHeader(original.headers, name);
    if (value !== null) headers.set(name, value);
  }
  return new Request(endpoint, {
    method: original.method,
    headers,
    ...(original.method === "GET" || original.method === "HEAD" || body === null ? {} : { body }),
    cache: "no-store",
    redirect: "manual",
  });
}

function safeMcpResponse(upstream: Response, credentials: string[]): Response {
  const headers = new Headers(noStoreHeaders());
  for (const name of MCP_RESPONSE_HEADERS) {
    const value = upstream.headers.get(name);
    if (value === null) continue;
    if (value.length > 4_096 || credentials.some((credential) => value.includes(credential))) {
      void cancelBody(upstream);
      throw new McpFailure(502, "credential_projection_blocked");
    }
    headers.set(name, value);
  }
  const bodyPermitted = upstream.status !== 101 && upstream.status !== 204
    && upstream.status !== 205 && upstream.status !== 304;
  return new Response(bodyPermitted && upstream.body
    ? credentialFilteringBody(upstream.body, credentials)
    : null, {
    status: upstream.status,
    headers,
  });
}

function authorizationCredentials(authorization: StoredAuthorization): string[] {
  return [authorization.accessToken, authorization.refreshToken, authorization.client.secret]
    .filter((value): value is string => Boolean(value));
}

function credentialFilteringBody(
  body: ReadableStream<Uint8Array>,
  credentials: string[],
): ReadableStream<Uint8Array> {
  const patterns = credentials.map((value) => new TextEncoder().encode(value));
  const hold = Math.max(0, ...patterns.map((pattern) => pattern.byteLength - 1));
  const reader = body.getReader();
  let tail = new Uint8Array();
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      while (true) {
        const { done, value } = await reader.read();
        if (done) {
          if (containsPattern(tail, patterns)) {
            controller.error(new Error("credential_projection_blocked"));
          } else {
            if (tail.byteLength > 0) controller.enqueue(tail);
            controller.close();
          }
          reader.releaseLock();
          return;
        }
        const combined = concatenate(tail, value);
        if (containsPattern(combined, patterns)) {
          await reader.cancel().catch(() => {});
          reader.releaseLock();
          controller.error(new Error("credential_projection_blocked"));
          return;
        }
        const emitLength = Math.max(0, combined.byteLength - hold);
        tail = combined.slice(emitLength);
        if (emitLength > 0) {
          controller.enqueue(combined.slice(0, emitLength));
          return;
        }
      }
    },
    async cancel(reason) {
      await reader.cancel(reason).catch(() => {});
      reader.releaseLock();
    },
  });
}

function captureReplayBody(body: ReadableStream<Uint8Array> | null): {
  stream: ReadableStream<Uint8Array> | null;
  replay: Promise<Uint8Array | undefined>;
} {
  if (!body) return { stream: null, replay: Promise.resolve(new Uint8Array()) };
  const [stream, replayStream] = body.tee();
  return { stream, replay: readReplayBody(replayStream) };
}

async function readReplayBody(body: ReadableStream<Uint8Array>): Promise<Uint8Array | undefined> {
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  let captureTimeout: ReturnType<typeof setTimeout> | undefined;
  const timedOut = new Promise<undefined>((resolve) => {
    captureTimeout = setTimeout(() => resolve(undefined), MAX_REPLAY_CAPTURE_MS);
  });
  try {
    while (true) {
      const next = await Promise.race([reader.read(), timedOut]);
      if (next === undefined) {
        void reader.cancel().catch(() => {});
        return undefined;
      }
      const { done, value } = next;
      if (done) break;
      total += value.byteLength;
      if (total > MAX_REPLAY_BODY_BYTES) {
        void reader.cancel().catch(() => {});
        return undefined;
      }
      chunks.push(value);
    }
  } finally {
    if (captureTimeout !== undefined) clearTimeout(captureTimeout);
    reader.releaseLock();
  }
  const result = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.byteLength; }
  return result;
}

async function oauthFetch(request: Request): Promise<Response> {
  try {
    return await fetch(request, { redirect: "manual", signal: AbortSignal.timeout(OAUTH_TIMEOUT_MS) });
  } catch { throw new McpFailure(503, "mcp_oauth_provider_unavailable"); }
}

async function mcpFetch(request: Request): Promise<Response> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), MCP_TIMEOUT_MS);
  try {
    return await fetch(request, { redirect: "manual", signal: controller.signal });
  } catch { throw new McpFailure(503, "mcp_upstream_unavailable"); }
  finally { clearTimeout(timeout); }
}

async function responseJson(response: Response): Promise<Record<string, unknown>> {
  const text = await readBoundedText(response, MAX_PROVIDER_BODY_BYTES);
  try {
    const value: unknown = JSON.parse(text);
    if (!isRecord(value)) throw new Error();
    return value;
  } catch { throw new McpFailure(502, "invalid_mcp_oauth_response"); }
}

function canonicalPublicEndpoint(value: string): boolean {
  const url = safePublicHttpsUrl(value);
  return Boolean(url && url.href === value && !url.search && url.pathname.startsWith("/"));
}

function safePublicHttpsUrl(value: string): URL | undefined {
  if (value.length > MAX_URL_BYTES) return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.port && !url.username && !url.password
      && !url.hash && !url.search && publicHostname(url.hostname)
      ? url
      : undefined;
  } catch { return undefined; }
}

function publicHostname(hostname: string): boolean {
  const value = hostname.toLowerCase().replace(/\.$/, "");
  if (!value || value === "localhost" || value.endsWith(".localhost")
    || reservedHostname(value)) return false;
  const ipv4 = value.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (ipv4) {
    const octets = ipv4.slice(1).map(Number);
    if (octets.some((part) => part > 255)) return false;
    return octets[0] !== 0 && octets[0] !== 10 && octets[0] !== 127
      && !(octets[0] === 169 && octets[1] === 254)
      && !(octets[0] === 172 && octets[1]! >= 16 && octets[1]! <= 31)
      && !(octets[0] === 192 && octets[1] === 168)
      && octets[0]! < 224;
  }
  if (value.startsWith("[") || value.includes(":")) return false;
  return value.includes(".");
}

function reservedHostname(value: string): boolean {
  const suffixes = [
    "local",
    "internal",
    "test",
    "invalid",
    "example",
    "onion",
    "home.arpa",
    "lan",
    "home",
    "corp",
  ];
  if (suffixes.some((suffix) => value === suffix || value.endsWith(`.${suffix}`))) return true;
  return ["example.com", "example.net", "example.org"].some(
    (hostname) => value === hostname || value.endsWith(`.${hostname}`),
  );
}

function wellKnownUrl(kind: string, target: URL): URL {
  const path = target.pathname === "/" ? "" : target.pathname;
  return new URL(`/.well-known/${kind}${path}`, target.origin);
}

function safeUrlArray(value: unknown): string[] | undefined {
  if (!Array.isArray(value) || value.length > 4) return undefined;
  const result: string[] = [];
  for (const item of value) {
    if (typeof item !== "string") return undefined;
    const url = safePublicHttpsUrl(item);
    if (!url) return undefined;
    result.push(url.href);
  }
  return result;
}

function metadataUrl(value: unknown): string | undefined {
  return typeof value === "string" ? safePublicHttpsUrl(value)?.href : undefined;
}

function optionalMetadataUrl(value: unknown): string | undefined {
  if (value === undefined) return undefined;
  const url = metadataUrl(value);
  if (!url) throw new McpFailure(502, "unsafe_mcp_oauth_metadata");
  return url;
}

function metadataStringArray(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || !validScopes(value)) {
    throw new McpFailure(502, "invalid_mcp_oauth_metadata");
  }
  return [...value];
}

function stringArrayField(value: unknown, key: string): string[] | undefined {
  if (!isRecord(value) || value[key] === undefined) return [];
  if (!Array.isArray(value[key]) || !validScopes(value[key])) return undefined;
  return [...value[key]];
}

export function validMcpConnectionMaterialization(value: unknown): boolean {
  return mcpMaterialization(value) !== undefined;
}

function mcpMaterialization(value: unknown): {
  endpoint: string;
  name: string;
  requestedScopes: string[];
} | undefined {
  const endpoint = stringField(value, "endpoint");
  const name = stringField(value, "name");
  const requestedScopes = stringArrayField(value, "scopes");
  return endpoint && name && name.trim() === name && name.length <= 128 && !hasControlCharacter(name)
    && requestedScopes !== undefined && canonicalMcpEndpoint(endpoint)
    ? { endpoint, name, requestedScopes }
    : undefined;
}

function canonicalMcpEndpoint(endpoint: string): boolean {
  try { return canonicalRemoteMcpTarget(endpoint).endpoint === endpoint; } catch { return false; }
}

function validScopes(value: unknown[]): value is string[] {
  return value.length <= 32 && value.every((scope) => typeof scope === "string"
    && scope.length > 0 && scope.length <= 256 && !/\s|[\u0000-\u001f\u007f]/.test(scope));
}

function validToken(value: string): boolean {
  return value.length > 0 && value.length <= MAX_TOKEN_BYTES
    && !/[\u0000-\u001f\u007f]/.test(value);
}

function validRedirectUri(value: string, env: McpConnectionBrokerEnv): boolean {
  if (value.length > MAX_URL_BYTES) return false;
  try {
    const url = new URL(value);
    const environment = env.ENVIRONMENT?.trim().toLowerCase();
    const local = environment === "local" || environment === "development" || environment === "test";
    return !url.username && !url.password && !url.hash
      && (url.protocol === "https:" || (local && url.protocol === "http:"
        && (url.hostname === "localhost" || url.hostname.endsWith(".localhost")
          || url.hostname === "127.0.0.1")));
  } catch { return false; }
}

function validReturnTo(value: string): boolean {
  return value.startsWith("/") && !value.startsWith("//") && value.length <= MAX_URL_BYTES;
}

function addClientSecret(parameters: URLSearchParams, client: OAuthClient): void {
  if (client.tokenEndpointAuthMethod === "client_secret_post" && client.secret) {
    parameters.set("client_secret", client.secret);
  }
}

function oauthFormHeaders(): HeadersInit {
  return { accept: "application/json", "content-type": "application/x-www-form-urlencoded" };
}

function boundedHeader(headers: Headers, name: string): string | null {
  const value = headers.get(name);
  if (value !== null && value.length > 4_096) {
    throw new McpFailure(431, "request_headers_too_large");
  }
  return value;
}

function randomBase64Url(bytes: number): string {
  return encodeBase64Url(crypto.getRandomValues(new Uint8Array(bytes)));
}

function encodeBase64Url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function concatenate(left: Uint8Array, right: Uint8Array): Uint8Array {
  const result = new Uint8Array(left.byteLength + right.byteLength);
  result.set(left, 0);
  result.set(right, left.byteLength);
  return result;
}

function containsPattern(value: Uint8Array, patterns: Uint8Array[]): boolean {
  return patterns.some((pattern) => indexOfBytes(value, pattern) !== -1);
}

function indexOfBytes(value: Uint8Array, pattern: Uint8Array): number {
  if (pattern.byteLength === 0 || pattern.byteLength > value.byteLength) return -1;
  outer: for (let index = 0; index <= value.byteLength - pattern.byteLength; index += 1) {
    for (let offset = 0; offset < pattern.byteLength; offset += 1) {
      if (value[index + offset] !== pattern[offset]) continue outer;
    }
    return index;
  }
  return -1;
}

async function readJson(request: Request, limit: number): Promise<Record<string, unknown> | undefined> {
  try {
    const value: unknown = JSON.parse(await readBoundedText(request, limit));
    return isRecord(value) ? value : undefined;
  } catch { return undefined; }
}

async function readBoundedText(message: Request | Response, limit: number): Promise<string> {
  if (!message.body) return "";
  const reader = message.body.getReader();
  const decoder = new TextDecoder();
  let bytes = 0;
  let text = "";
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) return text + decoder.decode();
      bytes += value.byteLength;
      if (bytes > limit) {
        await reader.cancel();
        throw new McpFailure(413, "body_too_large");
      }
      text += decoder.decode(value, { stream: true });
    }
  } finally { reader.releaseLock(); }
}

function stringField(value: unknown, key: string): string | undefined {
  return isRecord(value) && typeof value[key] === "string" && value[key].trim()
    ? value[key] as string
    : undefined;
}

function hasControlCharacter(value: string): boolean {
  return /[\u0000-\u001f\u007f]/.test(value);
}

function looksLikeUrl(value: string): boolean {
  return /^[A-Za-z][A-Za-z0-9+.-]*:/.test(value);
}

function unique<T>(values: T[]): T[] {
  return [...new Set(values)];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

async function cancelBody(response: Response): Promise<void> {
  try { await response.body?.cancel(); } catch { /* Best effort. */ }
}

function noStoreHeaders(): HeadersInit {
  return { "cache-control": "no-store", pragma: "no-cache", "x-content-type-options": "nosniff" };
}

function json(body: unknown, status: number): Response {
  return Response.json(body, { status, headers: noStoreHeaders() });
}

function jsonError(status: number, error: string): Response {
  return json({ error }, status);
}

class McpFailure extends Error {
  constructor(readonly status: number, readonly code: string) { super(code); }
}

function mcpFailure(error: unknown): McpFailure {
  return error instanceof McpFailure
    ? error
    : new McpFailure(503, "mcp_connection_broker_failed");
}
