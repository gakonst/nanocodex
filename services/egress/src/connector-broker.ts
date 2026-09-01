import { DurableObject } from "cloudflare:workers";

import {
  CredentialVault,
  type EncryptedEnvelope,
} from "./credential-vault";
import {
  buildGitHubAuthorizationUrl,
  buildGitHubIdentityRequest,
  buildGitHubTokenRefreshRequest,
  buildGitHubTokenRequest,
  decodeGitHubIdentity,
  decodeGitHubTokenResponse,
} from "./connectors/github";
import {
  buildGmailAuthorizationUrl,
  buildGmailIdentityRequest,
  buildGmailTokenRequest,
  decodeGmailIdentity,
  decodeGmailTokenResponse,
} from "./connectors/gmail";
import {
  buildGDriveAuthorizationUrl,
  buildGDriveIdentityRequest,
  buildGDriveTokenRequest,
  decodeGDriveIdentity,
  decodeGDriveTokenResponse,
} from "./connectors/gdrive";
import { canonicalConnectorPath } from "./connector-path";
import {
  McpConnectionOwner,
  type McpConnectionBrokerEnv,
} from "./mcp-connection-owner";
import {
  buildXAuthorizationUrl,
  buildXIdentityRequest,
  buildXRefreshRequest,
  buildXRevocationRequest,
  buildXTokenRequest,
  decodeXIdentity,
  decodeXTokenResponse,
} from "./connectors/x";

const STATE_KEY = "connector-state";
const PENDING_TTL_MS = 10 * 60_000;
const MAX_BODY_BYTES = 8 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES = 64 * 1024;
const MAX_CONNECTOR_URL_BYTES = 8 * 1024;
const MAX_CONNECTOR_RESPONSE_BYTES = 8 * 1024 * 1024;
const CONNECTOR_TIMEOUT_MS = 20_000;
const EXPIRY_SKEW_MS = 30_000;
const REDIRECT_STATUS = new Set([301, 302, 303, 307, 308]);
const CONNECTOR = /^(github|gmail|gdrive|x)$/;
const CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const CONNECTOR_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);

type ProviderRule = Readonly<{
  id: ConnectorId;
  origin: `https://${string}`;
  paths: readonly RegExp[];
}>;

const PROVIDER_RULES: readonly ProviderRule[] = [
  {
    id: "github",
    origin: "https://api.github.com",
    paths: [/^\//],
  },
  {
    id: "gmail",
    origin: "https://gmail.googleapis.com",
    paths: [/^\/gmail\/v1\/users\/me(?:\/|$)/],
  },
  {
    id: "gdrive",
    origin: "https://www.googleapis.com",
    paths: [/^\/drive\/v3(?:\/|$)/, /^\/upload\/drive\/v3(?:\/|$)/],
  },
  {
    id: "x",
    origin: "https://api.x.com",
    paths: [
      /^\/2\/tweets(?:\/|$)/,
      /^\/2\/users(?:\/|$)/,
      /^\/2\/lists(?:\/|$)/,
      /^\/2\/dm_(?:conversations|events)(?:\/|$)/,
      /^\/2\/media(?:\/|$)/,
    ],
  },
];

export type ConnectorId = "github" | "gmail" | "gdrive" | "x";

export interface ConnectorBrokerEnv extends McpConnectionBrokerEnv {
  GITHUB_OAUTH_CLIENT_ID?: string;
  GITHUB_OAUTH_CLIENT_SECRET?: string;
  GOOGLE_OAUTH_CLIENT_ID?: string;
  GOOGLE_OAUTH_CLIENT_SECRET?: string;
  X_OAUTH_CLIENT_ID?: string;
  X_OAUTH_CLIENT_SECRET?: string;
}

type StoredConnector = {
  accessToken: string;
  refreshToken?: string;
  expiresAt?: number;
  refreshExpiresAt?: number;
  scopes: string[];
  accountId: string;
  label: string;
  connectedAt: number;
};

type PendingAuthorization = {
  state: string;
  verifier: string;
  redirectUri: string;
  returnTo: string;
  expiresAt: number;
};

type ConnectorState = {
  version: 2;
  connectors: Partial<Record<ConnectorId, StoredConnector>>;
  googleConnections: Partial<Record<GoogleConnectorId, Record<string, StoredConnector>>>;
  pending: Partial<Record<ConnectorId, PendingAuthorization>>;
};

type LegacyConnectorState = {
  version: 1;
  connectors: Partial<Record<ConnectorId, StoredConnector>>;
  pending: Partial<Record<ConnectorId, PendingAuthorization>>;
};

type GoogleConnectorId = "gmail" | "gdrive";

type StoredRow = { envelope: EncryptedEnvelope };

export class UserConnectorBroker extends DurableObject<ConnectorBrokerEnv> {
  readonly #state: DurableObjectState;
  readonly #env: ConnectorBrokerEnv;
  readonly #vault: CredentialVault;
  readonly #mcpConnections: McpConnectionOwner;
  readonly #ready: Promise<void>;
  #connectors: ConnectorState = { version: 2, connectors: {}, googleConnections: {}, pending: {} };
  #tail: Promise<void> = Promise.resolve();

  constructor(state: DurableObjectState, env: ConnectorBrokerEnv) {
    super(state, env);
    this.#state = state;
    this.#env = env;
    this.#vault = new CredentialVault(env, `connectors/${state.id.toString()}`);
    this.#mcpConnections = new McpConnectionOwner(state, env);
    this.#ready = state.blockConcurrencyWhile(() => this.#initialize());
  }

  fetch(request: Request): Promise<Response> {
    return this.#exclusive(async () => {
      await this.#ready;
      return this.#dispatch(request);
    });
  }

  async #exclusive<T>(operation: () => Promise<T>): Promise<T> {
    const previous = this.#tail;
    let release!: () => void;
    this.#tail = new Promise<void>((resolve) => { release = resolve; });
    await previous;
    try { return await operation(); } finally { release(); }
  }

  async #initialize(): Promise<void> {
    const [row] = await Promise.all([
      this.#state.storage.get<StoredRow>(STATE_KEY),
      this.#mcpConnections.initialize(),
    ]);
    if (row) {
      const opened = await this.#vault.open<ConnectorState | LegacyConnectorState>(row.envelope);
      this.#connectors = migrateConnectorState(opened.value);
      if (opened.reseal || opened.value.version === 1) await this.#persist();
    }
  }

  async #dispatch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    let auditAction: ConnectorAuditAction | undefined;
    let auditConnector: ConnectorId | undefined;
    try {
      if (url.origin === "https://mcp-connections.internal") {
        return this.#mcpConnections.fetch(request);
      }
      const provider = providerRule(url);
      if (provider) {
        auditAction = "use";
        auditConnector = provider.id;
        const response = await this.#proxy(provider, request, url);
        connectorAudit("use", response.status >= 500 ? "error" : response.status >= 400 ? "deny" : "allow",
          provider.id, { status: response.status });
        return response;
      }
      if (PROVIDER_RULES.some((candidate) => candidate.origin === url.origin)
        || url.origin !== "https://connectors.internal") {
        return jsonError(403, "destination_denied");
      }
      if (request.method === "GET" && url.pathname === "/v1/status") {
        return json({ connectors: this.#publicStatus() }, 200);
      }
      const match = url.pathname.match(/^\/v1\/(github|gmail|gdrive|x)(?:\/(start|callback)|\/connections\/([A-Za-z0-9_-]{43}))?$/);
      const id = connectorId(match?.[1]);
      if (!id) return jsonError(404, "not_found");
      const operation = match?.[2];
      const connectionId = match?.[3];
      if (request.method === "POST" && operation === "start") {
        auditAction = "authorize_start";
        auditConnector = id;
        const result = json(await this.#start(id, request), 200);
        connectorAudit("authorize_start", "allow", id, { status: 200 });
        return result;
      }
      if (request.method === "POST" && operation === "callback") {
        auditAction = "authorize_callback";
        auditConnector = id;
        const callback = await this.#callback(id, request);
        connectorAudit("authorize_callback", callback.connected === true ? "allow" : "deny", id, {
          status: 200,
          connected: callback.connected === true,
        });
        return json(callback, 200);
      }
      if (request.method === "DELETE" && connectionId) {
        if (!isGoogleConnector(id)) return jsonError(404, "not_found");
        auditAction = "disconnect";
        auditConnector = id;
        const connector = this.#googleConnections(id)[connectionId];
        if (!connector) return jsonError(404, "connector_connection_not_found");
        const providerRevoked = await this.#revoke(id, connector);
        const disconnected = this.#deleteGoogleGrant(id, connectionId, connector);
        await this.#persist();
        connectorAudit("disconnect", "allow", id, {
          status: 204,
          provider_revoked: providerRevoked,
          disconnected_connectors: disconnected,
          connection_id: connectionId,
        });
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "DELETE" && operation === undefined) {
        auditAction = "disconnect";
        auditConnector = id;
        const google = isGoogleConnector(id) ? Object.entries(this.#googleConnections(id)) : [];
        const connector = isGoogleConnector(id) ? undefined : this.#connectors.connectors[id];
        let providerRevoked = false;
        for (const [, connection] of google) providerRevoked = await this.#revoke(id, connection) || providerRevoked;
        if (connector) providerRevoked = await this.#revoke(id, connector);
        const disconnected = isGoogleConnector(id)
          ? google.flatMap(([selectedId, connection]) => this.#deleteGoogleGrant(id, selectedId, connection))
          : this.#deleteConnectorGrant(id, connector);
        delete this.#connectors.pending[id];
        await this.#persist();
        connectorAudit("disconnect", "allow", id, {
          status: 204,
          provider_revoked: providerRevoked,
          disconnected_connectors: disconnected.length,
        });
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      return jsonError(405, "method_not_allowed");
    } catch (error) {
      const problem = connectorFailure(error);
      if (auditAction && auditConnector) {
        connectorAudit(auditAction, problem.status >= 500 ? "error" : "deny", auditConnector, {
          status: problem.status,
          code: problem.code,
        });
      }
      await this.#restoreDurableState();
      return problem.returnTo
        ? json({ error: problem.code, return_to: problem.returnTo }, problem.status)
        : jsonError(problem.status, problem.code);
    }
  }

  async #proxy(provider: ProviderRule, request: Request, url: URL): Promise<Response> {
    if (!CONNECTOR_METHODS.has(request.method)) {
      throw new ConnectorFailure(403, "method_denied");
    }
    if (url.href.length > MAX_CONNECTOR_URL_BYTES || url.username || url.password || url.hash) {
      throw new ConnectorFailure(403, "destination_denied");
    }
    if (!safeQuery(url.searchParams)) throw new ConnectorFailure(403, "destination_denied");
    const selectedConnectionId = request.headers.get("x-nanocodex-connector-connection");
    if (selectedConnectionId !== null && !CONNECTION_ID.test(selectedConnectionId)) {
      throw new ConnectorFailure(400, "connector_connection_invalid");
    }
    const connector = await this.#usableConnector(provider.id, selectedConnectionId ?? undefined);
    const headers = connectorRequestHeaders(request.headers, provider.id, connector.accessToken);
    let upstream: Response;
    try {
      upstream = await fetch(new Request(url, {
        method: request.method,
        headers,
        ...(request.method === "GET" || request.method === "HEAD" || !request.body
          ? {}
          : { body: request.body }),
        redirect: "manual",
      }), {
        redirect: "manual",
        signal: AbortSignal.timeout(CONNECTOR_TIMEOUT_MS),
      });
    } catch {
      throw new ConnectorFailure(503, "connector_provider_unavailable");
    }
    if (upstream.status === 401) {
      await upstream.body?.cancel();
      const effectiveConnectionId = selectedConnectionId
        ?? (isGoogleConnector(provider.id)
          ? Object.entries(this.#googleConnections(provider.id))
            .find(([, candidate]) => candidate.accountId === connector.accountId)?.[0]
          : undefined);
      this.#deleteSelectedGrant(provider.id, effectiveConnectionId, connector);
      await this.#persist();
      throw new ConnectorFailure(409, "connector_reauthentication_required");
    }
    if (REDIRECT_STATUS.has(upstream.status)) {
      await upstream.body?.cancel();
      throw new ConnectorFailure(502, "connector_redirect_blocked");
    }
    let body: Uint8Array | null;
    if (request.method === "HEAD" || !responseBodyPermitted(upstream.status)) {
      await upstream.body?.cancel();
      body = null;
    } else {
      body = await readBoundedBytes(upstream, MAX_CONNECTOR_RESPONSE_BYTES);
    }
    if (body && containsCredential(body, connector)) {
      throw new ConnectorFailure(502, "credential_projection_blocked");
    }
    return new Response(body, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers: connectorResponseHeaders(upstream.headers),
    });
  }

  async #usableConnector(id: ConnectorId, connectionId?: string): Promise<StoredConnector> {
    const selected = this.#selectConnector(id, connectionId);
    const connector = selected.connector;
    if (!connector) throw new ConnectorFailure(409, "connector_not_connected");
    if (connector.expiresAt === undefined || connector.expiresAt > Date.now() + EXPIRY_SKEW_MS) {
      return connector;
    }
    if (!connector.refreshToken) {
      return this.#rejectRefresh(id, connector, selected.connectionId);
    }
    if (connector.refreshExpiresAt !== undefined
      && connector.refreshExpiresAt <= Date.now() + EXPIRY_SKEW_MS) {
      this.#deleteSelectedGrant(id, selected.connectionId, connector);
      await this.#persist();
      throw new ConnectorFailure(409, "connector_reauthentication_required");
    }
    if (id === "github") return this.#refreshGitHubConnector(connector);
    if (id === "x") return this.#refreshXConnector(connector);
    return this.#refreshGoogleConnector(id, connector, selected.connectionId!);
  }

  async #refreshGitHubConnector(connector: StoredConnector): Promise<StoredConnector> {
    const credentials = providerCredentials("github", this.#env);
    const response = await providerFetch(buildGitHubTokenRefreshRequest({
      clientId: credentials.clientId,
      clientSecret: credentials.clientSecret,
      refreshToken: connector.refreshToken!,
    }));
    if (REDIRECT_STATUS.has(response.status) || !response.ok) {
      await response.body?.cancel();
      if (response.status === 400 || response.status === 401) {
        return this.#rejectRefresh("github", connector);
      }
      connectorAudit("refresh", "error", "github", {
        status: 503,
        code: "connector_provider_unavailable",
      });
      throw new ConnectorFailure(503, "connector_provider_unavailable");
    }
    let refreshed;
    try {
      refreshed = decodeGitHubTokenResponse(await providerJson(response));
    } catch {
      return this.#rejectRefresh("github", connector);
    }
    if (refreshed.expiresIn === undefined || refreshed.refreshToken === undefined) {
      return this.#rejectRefresh("github", connector);
    }
    const { refreshExpiresAt: _previousRefreshExpiry, ...retained } = connector;
    const next: StoredConnector = {
      ...retained,
      accessToken: refreshed.accessToken,
      refreshToken: refreshed.refreshToken,
      expiresAt: Date.now() + refreshed.expiresIn * 1_000,
      ...(refreshed.refreshTokenExpiresIn === undefined ? {} : {
        refreshExpiresAt: Date.now() + refreshed.refreshTokenExpiresIn * 1_000,
      }),
      scopes: [...refreshed.scopes],
    };
    this.#connectors.connectors.github = next;
    await this.#persist();
    connectorAudit("refresh", "allow", "github", { status: 200 });
    return next;
  }

  async #rejectRefresh(id: ConnectorId, connector: StoredConnector, connectionId?: string): Promise<never> {
    this.#deleteSelectedGrant(id, connectionId, connector);
    await this.#persist();
    connectorAudit("refresh", "deny", id, {
      status: 409,
      code: "connector_reauthentication_required",
    });
    throw new ConnectorFailure(409, "connector_reauthentication_required");
  }

  async #refreshXConnector(connector: StoredConnector): Promise<StoredConnector> {
    const credentials = providerCredentials("x", this.#env);
    const response = await providerFetch(buildXRefreshRequest(
      credentials.clientId,
      credentials.clientSecret,
      connector.refreshToken!,
    ));
    if (REDIRECT_STATUS.has(response.status) || !response.ok) {
      await response.body?.cancel();
      if (response.status === 400 || response.status === 401) {
        return this.#rejectRefresh("x", connector);
      }
      connectorAudit("refresh", "error", "x", {
        status: 503,
        code: "connector_provider_unavailable",
      });
      throw new ConnectorFailure(503, "connector_provider_unavailable");
    }
    let refreshed;
    try {
      refreshed = decodeXTokenResponse(await providerJson(response));
    } catch {
      return this.#rejectRefresh("x", connector);
    }
    const next: StoredConnector = {
      ...connector,
      accessToken: refreshed.accessToken,
      refreshToken: refreshed.refreshToken ?? connector.refreshToken!,
      expiresAt: Date.now() + refreshed.expiresIn * 1_000,
      scopes: [...refreshed.scopes],
    };
    this.#connectors.connectors.x = next;
    await this.#persist();
    connectorAudit("refresh", "allow", "x", { status: 200 });
    return next;
  }

  async #refreshGoogleConnector(
    id: "gmail" | "gdrive",
    connector: StoredConnector,
    connectionId: string,
  ): Promise<StoredConnector> {
    const credentials = providerCredentials(id, this.#env);
    const response = await providerFetch(new Request("https://oauth2.googleapis.com/token", {
      method: "POST",
      headers: {
        accept: "application/json",
        "content-type": "application/x-www-form-urlencoded",
      },
      body: new URLSearchParams({
        client_id: credentials.clientId,
        client_secret: credentials.clientSecret,
        refresh_token: connector.refreshToken!,
        grant_type: "refresh_token",
      }),
    }));
    if (REDIRECT_STATUS.has(response.status) || !response.ok) {
      await response.body?.cancel();
      if (response.status === 400 || response.status === 401) {
        return this.#rejectRefresh(id, connector, connectionId);
      }
      connectorAudit("refresh", "error", id, {
        status: 503,
        code: "connector_provider_unavailable",
      });
      throw new ConnectorFailure(503, "connector_provider_unavailable");
    }
    const refreshed = decodeGoogleRefresh(await providerJson(response));
    const next: StoredConnector = {
      ...connector,
      accessToken: refreshed.accessToken,
      expiresAt: Date.now() + refreshed.expiresIn * 1_000,
      ...(refreshed.scopes ? { scopes: refreshed.scopes } : {}),
    };
    this.#googleConnections(id)[connectionId] = next;
    await this.#persist();
    connectorAudit("refresh", "allow", id, { status: 200 });
    return next;
  }

  async #revoke(id: ConnectorId, connector: StoredConnector): Promise<boolean> {
    if (id === "x") {
      const credentials = providerCredentials(id, this.#env);
      const tokens = [...new Set([connector.refreshToken, connector.accessToken]
        .filter((token): token is string => Boolean(token)))];
      for (const token of tokens) {
        const response = await providerFetch(buildXRevocationRequest(
          credentials.clientId,
          token,
        ));
        await response.body?.cancel();
        const terminal = terminalRevocationStatus(response.status);
        connectorAudit("revoke", response.ok ? "allow" : terminal ? "deny" : "error", id, {
          status: response.status,
          token_kind: token === connector.refreshToken ? "refresh" : "access",
        });
        if (response.ok) return true;
        if (!terminal) {
          throw new ConnectorFailure(503, "connector_revocation_failed");
        }
      }
      return false;
    }
    const response = await providerFetch(revocationRequest(id, connector, this.#env));
    await response.body?.cancel();
    const revoked = id === "github" ? response.status === 204 : response.status === 200;
    if (!revoked) throw new ConnectorFailure(503, "connector_revocation_failed");
    return true;
  }

  #disconnectIds(id: ConnectorId, connector: StoredConnector): ConnectorId[] {
    if (id === "github" || id === "x") return [id];
    return (["gmail", "gdrive"] as const).filter((candidate) => (
      candidate === id
      || this.#connectors.connectors[candidate]?.accountId === connector.accountId
    ));
  }

  #deleteConnectorGrant(id: ConnectorId, connector?: StoredConnector): ConnectorId[] {
    const connectorIds = connector ? this.#disconnectIds(id, connector) : [id];
    for (const connectorId of connectorIds) {
      delete this.#connectors.connectors[connectorId];
    }
    return connectorIds;
  }

  #googleConnections(id: GoogleConnectorId): Record<string, StoredConnector> {
    return this.#connectors.googleConnections[id] ??= {};
  }

  #selectConnector(id: ConnectorId, connectionId?: string): { connector?: StoredConnector; connectionId?: string } {
    if (!isGoogleConnector(id)) {
      const connector = this.#connectors.connectors[id];
      return connector ? { connector } : {};
    }
    const connections = this.#googleConnections(id);
    if (connectionId) return { connector: connections[connectionId], connectionId };
    const usable = Object.entries(connections).filter(([, connector]) => this.#isUsable(connector));
    if (usable.length > 1) throw new ConnectorFailure(409, "connector_connection_required");
    return usable[0] ? { connectionId: usable[0][0], connector: usable[0][1] } : {};
  }

  #deleteSelectedGrant(id: ConnectorId, connectionId: string | undefined, connector: StoredConnector): void {
    if (isGoogleConnector(id) && connectionId) {
      this.#deleteGoogleGrant(id, connectionId, connector);
      return;
    }
    this.#deleteConnectorGrant(id, connector);
  }

  #deleteGoogleGrant(id: GoogleConnectorId, connectionId: string, connector: StoredConnector): number {
    let deleted = 0;
    for (const candidate of ["gmail", "gdrive"] as const) {
      for (const [candidateId, candidateConnector] of Object.entries(this.#googleConnections(candidate))) {
        if ((candidate === id && candidateId === connectionId)
          || candidateConnector.accountId === connector.accountId) {
          delete this.#googleConnections(candidate)[candidateId];
          deleted += 1;
        }
      }
    }
    return deleted;
  }

  #isUsable(connector: StoredConnector): boolean {
    const refreshable = connector.refreshToken
      && (connector.refreshExpiresAt === undefined
        || connector.refreshExpiresAt > Date.now() + EXPIRY_SKEW_MS);
    return connector.expiresAt === undefined
      || connector.expiresAt > Date.now() + EXPIRY_SKEW_MS
      || Boolean(refreshable);
  }

  #publicStatus(): Record<ConnectorId, Record<string, unknown>> {
    const status = (id: ConnectorId): Record<string, unknown> => {
      if (isGoogleConnector(id)) {
        const connections = Object.entries(this.#googleConnections(id))
          .filter(([, connector]) => this.#isUsable(connector))
          .sort(([, left], [, right]) => left.connectedAt - right.connectedAt)
          .map(([connectionId, connector]) => ({
            id: connectionId,
            account_id: connector.accountId,
            label: connector.label,
          }));
        return connections.length ? {
          connected: true,
          connections,
          ...(connections.length === 1 ? {
            connection_id: connections[0]!.id,
            account_id: connections[0]!.account_id,
            label: connections[0]!.label,
          } : {}),
        } : { connected: false, connections: [] };
      }
      const connector = this.#connectors.connectors[id];
      return connector && this.#isUsable(connector) ? {
        connected: true,
        account_id: connector.accountId,
        label: connector.label,
      } : { connected: false };
    };
    return {
      github: status("github"),
      gmail: status("gmail"),
      gdrive: status("gdrive"),
      x: status("x"),
    };
  }

  async #start(id: ConnectorId, request: Request): Promise<Record<string, unknown>> {
    const body = await readJson(request, MAX_BODY_BYTES);
    const redirectUri = stringField(body, "redirect_uri");
    const returnTo = stringField(body, "return_to");
    if (!redirectUri || !validRedirectUri(redirectUri, this.#env)
      || !returnTo || !validReturnTo(returnTo)) {
      throw new ConnectorFailure(400, "invalid_request");
    }
    const verifier = randomBase64Url(64);
    const state = randomBase64Url(32);
    const challenge = encodeBase64Url(new Uint8Array(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)),
    ));
    this.#connectors.pending[id] = {
      state,
      verifier,
      redirectUri,
      returnTo,
      expiresAt: Date.now() + PENDING_TTL_MS,
    };
    await this.#persist();
    return {
      authorization_url: authorizationUrl(id, this.#env, {
        redirectUri,
        state,
        codeChallenge: challenge,
      }).href,
    };
  }

  async #callback(id: ConnectorId, request: Request): Promise<Record<string, unknown>> {
    const body = await readJson(request, MAX_BODY_BYTES);
    const state = stringField(body, "state");
    const pending = this.#connectors.pending[id];
    if (!pending || pending.expiresAt <= Date.now() || !state || state !== pending.state) {
      throw new ConnectorFailure(400, "invalid_oauth_state");
    }
    delete this.#connectors.pending[id];
    await this.#persist();

    if (stringField(body, "error")) {
      return { connected: false, return_to: pending.returnTo };
    }
    const code = stringField(body, "code");
    if (!code) throw new ConnectorFailure(400, "authorization_code_missing");

    try {
      const tokenRequest = tokenExchangeRequest(id, this.#env, {
        code,
        codeVerifier: pending.verifier,
        redirectUri: pending.redirectUri,
      });
      const tokenResponse = await providerFetch(tokenRequest);
      if (!tokenResponse.ok) throw new ConnectorFailure(502, "connector_token_exchange_failed");
      let token: DecodedToken;
      try {
        token = decodeToken(id, await providerJson(tokenResponse));
      } catch (error) {
        if (error instanceof ConnectorFailure) throw error;
        throw new ConnectorFailure(502, "connector_token_response_invalid");
      }
      const identityResponse = await providerFetch(identityRequest(id, token.accessToken));
      if (!identityResponse.ok) throw new ConnectorFailure(502, "connector_identity_failed");
      let identity: { accountId: string; displayLabel: string };
      try {
        identity = decodeIdentity(id, await providerJson(identityResponse));
      } catch (error) {
        if (error instanceof ConnectorFailure) throw error;
        throw new ConnectorFailure(502, "connector_identity_response_invalid");
      }
      const next = {
        accessToken: token.accessToken,
        ...(token.refreshToken ? { refreshToken: token.refreshToken } : {}),
        ...(token.expiresIn ? { expiresAt: Date.now() + token.expiresIn * 1_000 } : {}),
        ...(token.refreshExpiresIn
          ? { refreshExpiresAt: Date.now() + token.refreshExpiresIn * 1_000 }
          : {}),
        scopes: [...token.scopes],
        accountId: identity.accountId,
        label: identity.displayLabel,
        connectedAt: Date.now(),
      };
      let connectionId: string | undefined;
      if (isGoogleConnector(id)) {
        connectionId = Object.entries(this.#googleConnections(id))
          .find(([, connector]) => connector.accountId === identity.accountId)?.[0]
          ?? randomBase64Url(32);
        this.#googleConnections(id)[connectionId] = next;
      } else {
        this.#connectors.connectors[id] = next;
      }
      await this.#persist();
      return { connected: true, return_to: pending.returnTo, ...(connectionId ? { connection_id: connectionId } : {}) };
    } catch (error) {
      const problem = connectorFailure(error);
      throw new ConnectorFailure(problem.status, problem.code, pending.returnTo);
    }
  }

  async #persist(): Promise<void> {
    await this.#state.storage.put(STATE_KEY, {
      envelope: await this.#vault.seal(this.#connectors),
    } satisfies StoredRow);
  }

  async #restoreDurableState(): Promise<void> {
    try {
      const row = await this.#state.storage.get<StoredRow>(STATE_KEY);
      this.#connectors = row
        ? migrateConnectorState((await this.#vault.open<ConnectorState | LegacyConnectorState>(row.envelope)).value)
        : { version: 2, connectors: {}, googleConnections: {}, pending: {} };
    } catch {
      this.#connectors = { version: 2, connectors: {}, googleConnections: {}, pending: {} };
    }
  }
}

type AuthorizationFields = { redirectUri: string; state: string; codeChallenge: string };
type ExchangeFields = { redirectUri: string; code: string; codeVerifier: string };
type DecodedToken = {
  accessToken: string;
  refreshToken?: string;
  expiresIn?: number;
  refreshExpiresIn?: number;
  scopes: readonly string[];
};

function authorizationUrl(
  id: ConnectorId,
  env: ConnectorBrokerEnv,
  fields: AuthorizationFields,
): URL {
  const clientId = providerCredentials(id, env).clientId;
  if (id === "github") return buildGitHubAuthorizationUrl({ clientId, ...fields });
  if (id === "gmail") return buildGmailAuthorizationUrl({ clientId, ...fields });
  if (id === "x") return buildXAuthorizationUrl({ clientId, ...fields });
  return buildGDriveAuthorizationUrl({ clientId, ...fields });
}

function tokenExchangeRequest(
  id: ConnectorId,
  env: ConnectorBrokerEnv,
  fields: ExchangeFields,
): Request {
  const credentials = providerCredentials(id, env);
  if (id === "github") return buildGitHubTokenRequest({
    clientId: credentials.clientId,
    clientSecret: credentials.clientSecret,
    ...fields,
  });
  if (id === "gmail") return buildGmailTokenRequest({
    clientId: credentials.clientId,
    clientSecret: credentials.clientSecret,
    ...fields,
  });
  if (id === "x") return buildXTokenRequest({
    clientId: credentials.clientId,
    clientSecret: credentials.clientSecret,
    ...fields,
  });
  return buildGDriveTokenRequest({
    clientId: credentials.clientId,
    clientSecret: credentials.clientSecret,
    authorizationCode: fields.code,
    redirectUri: fields.redirectUri,
    codeVerifier: fields.codeVerifier,
  });
}

function decodeToken(id: ConnectorId, value: unknown): DecodedToken {
  if (id === "github") {
    const token = decodeGitHubTokenResponse(value);
    return {
      accessToken: token.accessToken,
      ...(token.refreshToken ? { refreshToken: token.refreshToken } : {}),
      ...(token.expiresIn ? { expiresIn: token.expiresIn } : {}),
      ...(token.refreshTokenExpiresIn
        ? { refreshExpiresIn: token.refreshTokenExpiresIn }
        : {}),
      scopes: token.scopes,
    };
  }
  if (id === "gmail") {
    const token = decodeGmailTokenResponse(value);
    return {
      accessToken: token.accessToken,
      ...(token.refreshToken ? { refreshToken: token.refreshToken } : {}),
      expiresIn: token.expiresIn,
      scopes: token.scopes,
    };
  }
  if (id === "x") {
    const token = decodeXTokenResponse(value);
    if (!token.refreshToken) {
      throw new ConnectorFailure(502, "connector_token_response_invalid");
    }
    return {
      accessToken: token.accessToken,
      ...(token.refreshToken ? { refreshToken: token.refreshToken } : {}),
      expiresIn: token.expiresIn,
      scopes: token.scopes,
    };
  }
  const token = decodeGDriveTokenResponse(value);
  return {
    accessToken: token.accessToken,
    ...(token.refreshToken ? { refreshToken: token.refreshToken } : {}),
    expiresIn: token.expiresInSeconds,
    scopes: token.grantedScopes,
  };
}

function identityRequest(id: ConnectorId, accessToken: string): Request {
  if (id === "github") return buildGitHubIdentityRequest(accessToken);
  if (id === "gmail") return buildGmailIdentityRequest(accessToken);
  if (id === "x") return buildXIdentityRequest(accessToken);
  return buildGDriveIdentityRequest(accessToken);
}

function revocationRequest(
  id: ConnectorId,
  connector: StoredConnector,
  env: ConnectorBrokerEnv,
): Request {
  if (id === "github") {
    const credentials = providerCredentials(id, env);
    return new Request(
      `https://api.github.com/applications/${encodeURIComponent(credentials.clientId)}/token`,
      {
        method: "DELETE",
        headers: {
          accept: "application/vnd.github+json",
          authorization: `Basic ${btoa(`${credentials.clientId}:${credentials.clientSecret}`)}`,
          "content-type": "application/json",
          "user-agent": "nanocodex-connector-broker",
          "x-github-api-version": "2026-03-10",
        },
        body: JSON.stringify({ access_token: connector.accessToken }),
      },
    );
  }
  return new Request("https://oauth2.googleapis.com/revoke", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ token: connector.refreshToken ?? connector.accessToken }),
  });
}

function terminalRevocationStatus(status: number): boolean {
  return status >= 400 && status < 500
    && status !== 408
    && status !== 425
    && status !== 429;
}

function decodeIdentity(id: ConnectorId, value: unknown): { accountId: string; displayLabel: string } {
  if (id === "github") return decodeGitHubIdentity(value);
  if (id === "gmail") return decodeGmailIdentity(value);
  if (id === "x") return decodeXIdentity(value);
  return decodeGDriveIdentity(value);
}

function providerCredentials(
  id: ConnectorId,
  env: ConnectorBrokerEnv,
): { clientId: string; clientSecret: string } {
  const clientId = (id === "github" ? env.GITHUB_OAUTH_CLIENT_ID
    : id === "x" ? env.X_OAUTH_CLIENT_ID
    : env.GOOGLE_OAUTH_CLIENT_ID)?.trim();
  const clientSecret = (id === "github"
    ? env.GITHUB_OAUTH_CLIENT_SECRET
    : id === "x" ? env.X_OAUTH_CLIENT_SECRET
    : env.GOOGLE_OAUTH_CLIENT_SECRET)?.trim();
  if (!clientId || !clientSecret) throw new ConnectorFailure(503, "connector_not_configured");
  return { clientId, clientSecret };
}

async function providerFetch(request: Request): Promise<Response> {
  try {
    return await fetch(request, { redirect: "manual", signal: AbortSignal.timeout(30_000) });
  } catch { throw new ConnectorFailure(503, "connector_provider_unavailable"); }
}

async function providerJson(response: Response): Promise<Record<string, unknown>> {
  const text = await readBoundedText(response, MAX_PROVIDER_RESPONSE_BYTES);
  try {
    const value: unknown = JSON.parse(text);
    if (!isRecord(value)) throw new Error();
    return value;
  } catch { throw new ConnectorFailure(502, "invalid_connector_provider_response"); }
}

function decodeGoogleRefresh(value: unknown): {
  accessToken: string;
  expiresIn: number;
  scopes?: string[];
} {
  if (!isRecord(value) || typeof value.access_token !== "string" || !value.access_token
    || value.token_type !== "Bearer" || typeof value.expires_in !== "number"
    || !Number.isSafeInteger(value.expires_in) || value.expires_in <= 0) {
    throw new ConnectorFailure(409, "connector_reauthentication_required");
  }
  if (value.scope !== undefined && (typeof value.scope !== "string" || !value.scope.trim())) {
    throw new ConnectorFailure(409, "connector_reauthentication_required");
  }
  return {
    accessToken: value.access_token,
    expiresIn: value.expires_in,
    ...(typeof value.scope === "string" ? { scopes: value.scope.trim().split(/\s+/) } : {}),
  };
}

function providerRule(url: URL): ProviderRule | undefined {
  return PROVIDER_RULES.find((candidate) => candidate.origin === url.origin
    && canonicalConnectorPath(candidate.id, url.pathname)
    && candidate.paths.some((path) => path.test(url.pathname)));
}

function safeQuery(parameters: URLSearchParams): boolean {
  const forbidden = new Set(["access_token", "api_key", "authorization", "key", "oauth_token"]);
  let count = 0;
  for (const [name, value] of parameters) {
    count += 1;
    if (count > 64 || name.length > 128 || value.length > 4_096
      || forbidden.has(name.toLowerCase())) return false;
  }
  return true;
}

function connectorRequestHeaders(
  caller: Headers,
  id: ConnectorId,
  accessToken: string,
): Headers {
  const headers = new Headers({
    accept: boundedCallerHeader(caller, "accept") ?? "application/json",
    authorization: `Bearer ${accessToken}`,
  });
  for (const name of [
    "content-range",
    "content-type",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
  ])
    if (caller.has(name)) headers.set(name, boundedCallerHeader(caller, name)!);
  if (id === "github") {
    headers.set("x-github-api-version", "2026-03-10");
    headers.set("user-agent", "nanocodex-connector-broker");
  }
  return headers;
}

function boundedCallerHeader(headers: Headers, name: string): string | null {
  const value = headers.get(name);
  if (value !== null && value.length > 1_024) {
    throw new ConnectorFailure(431, "request_headers_too_large");
  }
  return value;
}

function responseBodyPermitted(status: number): boolean {
  return status !== 101 && status !== 204 && status !== 205 && status !== 304;
}

function connectorResponseHeaders(upstream: Headers): Headers {
  const headers = new Headers(noStoreHeaders());
  for (const name of [
    "accept-ranges",
    "content-range",
    "content-type",
    "etag",
    "last-modified",
    "retry-after",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-ratelimit-resource",
  ]) if (upstream.has(name)) headers.set(name, upstream.get(name)!);
  return headers;
}

async function readBoundedBytes(response: Response, limit: number): Promise<Uint8Array> {
  const declared = response.headers.get("content-length");
  if (declared !== null) {
    const size = Number(declared);
    if (!/^(?:0|[1-9][0-9]*)$/.test(declared) || !Number.isSafeInteger(size) || size > limit) {
      await response.body?.cancel();
      throw new ConnectorFailure(502, "connector_response_too_large");
    }
  }
  if (!response.body) return new Uint8Array();
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > limit) {
        await reader.cancel();
        throw new ConnectorFailure(502, "connector_response_too_large");
      }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  const body = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
  return body;
}

function containsCredential(body: Uint8Array, connector: StoredConnector): boolean {
  const credentials = [connector.accessToken, connector.refreshToken].filter(
    (value): value is string => Boolean(value),
  );
  const decoded = new TextDecoder().decode(body);
  return credentials.some((credential) => decoded.includes(credential));
}

function validRedirectUri(value: string, env: ConnectorBrokerEnv): boolean {
  try {
    const url = new URL(value);
    const environment = env.ENVIRONMENT?.trim().toLowerCase();
    const local = environment === "local" || environment === "development" || environment === "test";
    return !url.username && !url.password && !url.hash
      && (url.protocol === "https:" || (local && url.protocol === "http:"
        && (url.hostname === "localhost"
          || url.hostname.endsWith(".localhost")
          || url.hostname === "127.0.0.1")));
  } catch { return false; }
}

function validReturnTo(value: string): boolean {
  return value.startsWith("/") && !value.startsWith("//") && value.length <= 2_048;
}

function randomBase64Url(bytes: number): string {
  return encodeBase64Url(crypto.getRandomValues(new Uint8Array(bytes)));
}

function encodeBase64Url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
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
        throw new ConnectorFailure(413, "body_too_large");
      }
      text += decoder.decode(value, { stream: true });
    }
  } finally { reader.releaseLock(); }
}

function stringField(value: unknown, key: string): string | undefined {
  return isRecord(value) && typeof value[key] === "string" && value[key].trim()
    ? value[key] as string : undefined;
}

function connectorId(value: string | undefined): ConnectorId | undefined {
  return value && CONNECTOR.test(value) ? value as ConnectorId : undefined;
}

function isGoogleConnector(id: ConnectorId): id is GoogleConnectorId {
  return id === "gmail" || id === "gdrive";
}

function migrateConnectorState(value: ConnectorState | LegacyConnectorState): ConnectorState {
  if (value.version === 2) return value;
  const connectors = { ...value.connectors };
  const googleConnections: ConnectorState["googleConnections"] = {};
  for (const id of ["gmail", "gdrive"] as const) {
    const connector = connectors[id];
    if (!connector) continue;
    delete connectors[id];
    googleConnections[id] = { [randomBase64Url(32)]: connector };
  }
  return { version: 2, connectors, googleConnections, pending: value.pending };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

type ConnectorAuditAction =
  | "authorize_start"
  | "authorize_callback"
  | "disconnect"
  | "refresh"
  | "revoke"
  | "use";

function connectorAudit(
  action: ConnectorAuditAction,
  outcome: "allow" | "deny" | "error",
  connector: ConnectorId,
  detail: Readonly<Record<string, boolean | number | string>>,
): void {
  console.log({
    type: "connector.audit",
    action,
    outcome,
    connector,
    ...detail,
  });
}

class ConnectorFailure extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    readonly returnTo?: string,
  ) { super(code); }
}

function connectorFailure(error: unknown): ConnectorFailure {
  return error instanceof ConnectorFailure
    ? error
    : new ConnectorFailure(503, "connector_broker_failed");
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
