import { DurableObject } from "cloudflare:workers";
import { credentialFilteringBody } from "./credential-stream";

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
  buildGoogleAuthorizationUrl,
  buildGoogleIdentityRequest,
  buildGoogleTokenRequest,
  decodeGoogleIdentity,
  decodeGoogleTokenResponse,
  googleCapabilities,
  type GoogleCapabilityId,
} from "./connectors/google";
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
import {
  buildSlackAuthorizationUrl,
  buildSlackRevocationRequest,
  buildSlackTokenRefreshRequest,
  buildSlackTokenRequest,
  decodeSlackRefreshResponse,
  decodeSlackTokenResponse,
  slackConnectionLabel,
} from "./connectors/slack";

const STATE_KEY = "connector-state";
const PENDING_TTL_MS = 10 * 60_000;
const MAX_BODY_BYTES = 8 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES = 64 * 1024;
const MAX_CONNECTOR_URL_BYTES = 8 * 1024;
const MAX_SLACK_REQUEST_BYTES = 1024 * 1024;
const EXPIRY_SKEW_MS = 30_000;
const REVOCATION_RETRY_BASE_MS = 30_000;
const REVOCATION_RETRY_MAX_MS = 60 * 60_000;
const REDIRECT_STATUS = new Set([301, 302, 303, 307, 308]);
const CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const PROVIDER = /^(github|google|slack|x)$/;
const CONNECTOR_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);

type ProviderRule = Readonly<{
  id: ConnectorId;
  provider: OAuthProviderId;
  origin: `https://${string}`;
  paths: readonly RegExp[];
}>;

const PROVIDER_RULES: readonly ProviderRule[] = [
  {
    id: "github",
    provider: "github",
    origin: "https://github.com",
    paths: [/^\/[A-Za-z0-9_-]+\/[A-Za-z0-9_.-]+\/(?:info\/refs|git-upload-pack|git-receive-pack)$/],
  },
  {
    id: "github",
    provider: "github",
    origin: "https://api.github.com",
    paths: [/^\//],
  },
  {
    id: "gmail",
    provider: "google",
    origin: "https://gmail.googleapis.com",
    paths: [/^\/gmail\/v1\/users\/me(?:\/|$)/],
  },
  {
    id: "gdrive",
    provider: "google",
    origin: "https://www.googleapis.com",
    paths: [/^\/drive\/v3(?:\/|$)/, /^\/upload\/drive\/v3(?:\/|$)/],
  },
  {
    id: "gcalendar",
    provider: "google",
    origin: "https://www.googleapis.com",
    paths: [/^\/calendar\/v3(?:\/|$)/],
  },
  {
    id: "gcalendar",
    provider: "google",
    origin: "https://calendar.googleapis.com",
    paths: [/^\/calendar\/v3(?:\/|$)/],
  },
  {
    id: "gtasks",
    provider: "google",
    origin: "https://tasks.googleapis.com",
    paths: [/^\/tasks\/v1(?:\/|$)/],
  },
  {
    id: "gdocs",
    provider: "google",
    origin: "https://docs.googleapis.com",
    paths: [/^\/v1\/documents(?:\/|$)/],
  },
  {
    id: "gsheets",
    provider: "google",
    origin: "https://sheets.googleapis.com",
    paths: [/^\/v4\/spreadsheets(?:\/|$)/],
  },
  {
    id: "gslides",
    provider: "google",
    origin: "https://slides.googleapis.com",
    paths: [/^\/v1\/presentations(?:\/|$)/],
  },
  {
    id: "gcontacts",
    provider: "google",
    origin: "https://people.googleapis.com",
    paths: [/^\/v1\/(?:people|contactGroups|otherContacts)(?:\/|:|$)/],
  },
  {
    id: "x",
    provider: "x",
    origin: "https://api.x.com",
    paths: [
      /^\/2\/tweets(?:\/|$)/,
      /^\/2\/users(?:\/|$)/,
      /^\/2\/lists(?:\/|$)/,
      /^\/2\/dm_(?:conversations|events)(?:\/|$)/,
      /^\/2\/media(?:\/|$)/,
    ],
  },
  {
    id: "slack",
    provider: "slack",
    origin: "https://slack.com",
    paths: [/^\/api\/(?!auth\.revoke$)[A-Za-z0-9._-]+$/],
  },
];

export type ConnectorId = "github" | GoogleCapabilityId | "slack" | "x";
export type OAuthProviderId = "github" | "google" | "slack" | "x";

export interface ConnectorBrokerEnv extends McpConnectionBrokerEnv {
  GITHUB_OAUTH_CLIENT_ID?: string;
  GITHUB_OAUTH_CLIENT_SECRET?: string;
  GOOGLE_OAUTH_CLIENT_ID?: string;
  GOOGLE_OAUTH_CLIENT_SECRET?: string;
  X_OAUTH_CLIENT_ID?: string;
  X_OAUTH_CLIENT_SECRET?: string;
  SLACK_OAUTH_CLIENT_ID?: string;
  SLACK_OAUTH_CLIENT_SECRET?: string;
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
  teamId?: string;
  teamName?: string;
  userId?: string;
};

type PendingAuthorization = {
  state: string;
  verifier: string;
  redirectUri: string;
  returnTo: string;
  accountHint?: string;
  expiresAt: number;
};

type ConnectorState = {
  version: 2;
  connections: Partial<Record<OAuthProviderId, Record<string, StoredConnector>>>;
  pending: Partial<Record<OAuthProviderId, PendingAuthorization>>;
  revocations?: PendingRevocation[];
};

type LegacyConnectorState = {
  version: 1;
  connectors: Partial<Record<"github" | "gmail" | "gdrive" | "x", StoredConnector>>;
  slack?: Record<string, StoredConnector>;
  pending: Partial<Record<"github" | "gmail" | "gdrive" | "slack" | "x", PendingAuthorization>>;
  revocations?: LegacyPendingRevocation[];
};

type PendingRevocation = {
  provider: OAuthProviderId;
  connector: StoredConnector;
  attempts: number;
};

type LegacyPendingRevocation = {
  id: "github" | "gmail" | "gdrive" | "x";
  connector: StoredConnector;
  attempts: number;
};

type StoredRow = { envelope: EncryptedEnvelope };

export class UserConnectorBroker extends DurableObject<ConnectorBrokerEnv> {
  readonly #state: DurableObjectState;
  readonly #env: ConnectorBrokerEnv;
  readonly #vault: CredentialVault;
  readonly #mcpConnections: McpConnectionOwner;
  readonly #ready: Promise<void>;
  #connectors: ConnectorState = { version: 2, connections: {}, pending: {} };
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

  alarm(): Promise<void> {
    return this.#exclusive(async () => {
      await this.#ready;
      await this.#retryPendingRevocations();
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
    if (this.#pendingRevocations().length > 0) {
      await this.#schedulePendingRevocations();
    }
  }

  async #dispatch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    let auditAction: ConnectorAuditAction | undefined;
    let auditConnector: ConnectorId | OAuthProviderId | undefined;
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
      const match = url.pathname.match(
        /^\/v1\/(github|google|gmail|gdrive|slack|x)(?:\/(start|callback)|\/connections\/([A-Za-z0-9_-]{43}))?$/,
      );
      const controlId = match?.[1];
      const id = oauthProviderId(controlId);
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
        auditAction = "disconnect";
        auditConnector = id;
        const connector = this.#connections(id)[connectionId];
        if (!connector) return jsonError(404, "connector_connection_not_found");
        const providerRevoked = await this.#revoke(id, connector);
        delete this.#connections(id)[connectionId];
        await this.#persist();
        connectorAudit("disconnect", "allow", id, {
          status: 204,
          provider_revoked: providerRevoked,
          disconnected_connectors: 1,
          connection_id: connectionId,
        });
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "DELETE" && operation === undefined) {
        auditAction = "disconnect";
        auditConnector = id;
        const selected = Object.entries(this.#connections(id)).filter(([, connector]) => (
          controlId !== "gmail" && controlId !== "gdrive"
            ? true
            : capabilitiesFor(id, connector.scopes).includes(controlId)
        ));
        let providerRevoked = false;
        for (const [selectedId, connector] of selected) {
          providerRevoked = await this.#revoke(id, connector) || providerRevoked;
          delete this.#connections(id)[selectedId];
        }
        delete this.#connectors.pending[id];
        await this.#persist();
        connectorAudit("disconnect", "allow", id, {
          status: 204,
          provider_revoked: providerRevoked,
          disconnected_connectors: selected.length,
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
    const archiveRepository = provider.id === "github" && request.method === "GET"
      && url.origin === "https://api.github.com"
      ? /^\/repos\/([A-Za-z0-9_-]+\/[A-Za-z0-9_.-]+)\/tarball(?:\/[^/]+)?$/.exec(url.pathname)?.[1]
      : undefined;
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
    let selected: { connectionId: string; connector: StoredConnector } | undefined;
    try {
      selected = await this.#usableConnector(provider, selectedConnectionId ?? undefined);
    } catch (error) {
      // Public Git remains usable before an account connects GitHub. Never
      // fall back when an exact connection was selected or has expired.
      if ((provider.origin !== "https://github.com" && archiveRepository === undefined) || selectedConnectionId !== null
        || !(error instanceof ConnectorFailure) || error.code !== "connector_not_connected") throw error;
    }
    const connector = selected?.connector;
    const headers = connectorRequestHeaders(request.headers, provider.id, connector?.accessToken);
    if (provider.origin === "https://github.com" && connector) {
      headers.set("authorization", `Basic ${btoa(`x-access-token:${connector.accessToken}`)}`);
    }
    const requestBody = provider.provider === "slack"
      ? await slackRequestBody(request)
      : request.body;
    let upstream: Response;
    const archiveCredentials: string[] = [];
    try {
      upstream = await fetch(new Request(url, {
        method: request.method,
        headers,
        ...(request.method === "GET" || request.method === "HEAD" || !requestBody
          ? {}
          : { body: requestBody }),
        redirect: "manual",
      }), {
        redirect: "manual",
        signal: request.signal,
      });
    } catch {
      throw new ConnectorFailure(503, "connector_provider_unavailable");
    }
    if (upstream.status === 401 && selected) {
      await upstream.body?.cancel();
      delete this.#connections(provider.provider)[selected.connectionId];
      await this.#persist();
      throw new ConnectorFailure(409, "connector_reauthentication_required");
    }
    if (REDIRECT_STATUS.has(upstream.status)) {
      // GitHub's archive API returns a codeload URL. Resolve that one redirect
      // inside the broker; neither OAuth credentials nor signed URLs reach tools.
      const location = upstream.headers.get("location");
      await upstream.body?.cancel();
      let target: URL | undefined;
      try { if (location) target = new URL(location); } catch { /* Fail closed below. */ }
      if (!archiveRepository || !target || target.origin !== "https://codeload.github.com"
        || target.username || target.password || target.hash || target.href.length > MAX_CONNECTOR_URL_BYTES
        || !target.pathname.startsWith(`/${archiveRepository}/legacy.tar.gz/`)) {
        throw new ConnectorFailure(502, "connector_redirect_blocked");
      }
      for (const name of ["token", "access_token"]) {
        const value = target.searchParams.get(name);
        if (value) archiveCredentials.push(value);
      }
      try {
        upstream = await fetch(target, {
          method: "GET", redirect: "manual", signal: request.signal,
          headers: { "user-agent": "nanocodex-connector-broker", accept: "application/gzip" },
        });
      } catch {
        throw new ConnectorFailure(503, "connector_provider_unavailable");
      }
      if (REDIRECT_STATUS.has(upstream.status)) {
        await upstream.body?.cancel();
        throw new ConnectorFailure(502, "connector_redirect_blocked");
      }
    }
    let body: ReadableStream<Uint8Array> | null;
    if (request.method === "HEAD" || !responseBodyPermitted(upstream.status)) {
      await upstream.body?.cancel();
      body = null;
    } else {
      body = upstream.body;
    }
    const credentials = [...archiveCredentials, ...(connector ? [connector.accessToken, connector.refreshToken,
      headers.get("authorization")?.split(" ", 2)[1]]
      .filter((value): value is string => Boolean(value)) : [])];
    const responseHeaders = connectorResponseHeaders(upstream.headers);
    for (const value of responseHeaders.values()) {
      if (credentials.some((credential) => value.includes(credential))) {
        await body?.cancel();
        throw new ConnectorFailure(502, "credential_projection_blocked");
      }
    }
    return new Response(body && credentials.length ? credentialFilteringBody(body, credentials) : body, {
      status: upstream.status,
      headers: responseHeaders,
    });
  }

  async #usableConnector(
    rule: ProviderRule,
    connectionId?: string,
  ): Promise<{ connectionId: string; connector: StoredConnector }> {
    const connections = this.#connections(rule.provider);
    let selected: [string, StoredConnector] | undefined;
    if (connectionId) {
      const connector = connections[connectionId];
      if (!connector || !capabilitiesFor(rule.provider, connector.scopes).includes(rule.id)) {
        throw new ConnectorFailure(404, "connector_connection_not_found");
      }
      selected = [connectionId, connector];
    } else {
      const candidates = Object.entries(connections).filter(([id, connector]) => (
        CONNECTION_ID.test(id) && this.#isUsable(connector)
          && capabilitiesFor(rule.provider, connector.scopes).includes(rule.id)
      ));
      if (candidates.length > 1) throw new ConnectorFailure(409, "connector_connection_required");
      selected = candidates[0];
    }
    if (!selected) throw new ConnectorFailure(409, "connector_not_connected");
    const [selectedId, connector] = selected;
    if (connector.expiresAt === undefined || connector.expiresAt > Date.now() + EXPIRY_SKEW_MS) {
      return { connectionId: selectedId, connector };
    }
    if (!connector.refreshToken) {
      return this.#rejectRefresh(rule.provider, selectedId, connector);
    }
    if (connector.refreshExpiresAt !== undefined
      && connector.refreshExpiresAt <= Date.now() + EXPIRY_SKEW_MS) {
      delete connections[selectedId];
      await this.#persist();
      throw new ConnectorFailure(409, "connector_reauthentication_required");
    }
    const refreshed = rule.provider === "github"
      ? await this.#refreshGitHubConnector(selectedId, connector)
      : rule.provider === "x"
        ? await this.#refreshXConnector(selectedId, connector)
        : rule.provider === "slack"
          ? await this.#refreshSlackConnector(selectedId, connector)
          : await this.#refreshGoogleConnector(selectedId, connector);
    if (!capabilitiesFor(rule.provider, refreshed.scopes).includes(rule.id)) {
      throw new ConnectorFailure(409, "connector_capability_not_granted");
    }
    return { connectionId: selectedId, connector: refreshed };
  }

  async #refreshGitHubConnector(connectionId: string, connector: StoredConnector): Promise<StoredConnector> {
    const credentials = providerCredentials("github", this.#env);
    const response = await providerFetch(buildGitHubTokenRefreshRequest({
      clientId: credentials.clientId,
      clientSecret: credentials.clientSecret,
      refreshToken: connector.refreshToken!,
    }));
    if (REDIRECT_STATUS.has(response.status) || !response.ok) {
      await response.body?.cancel();
      if (response.status === 400 || response.status === 401) {
        return this.#rejectRefresh("github", connectionId, connector);
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
      return this.#rejectRefresh("github", connectionId, connector);
    }
    if (refreshed.expiresIn === undefined || refreshed.refreshToken === undefined) {
      return this.#rejectRefresh("github", connectionId, connector);
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
    this.#connections("github")[connectionId] = next;
    await this.#persist();
    connectorAudit("refresh", "allow", "github", { status: 200 });
    return next;
  }

  async #rejectRefresh(
    provider: OAuthProviderId,
    connectionId: string,
    _connector: StoredConnector,
  ): Promise<never> {
    delete this.#connections(provider)[connectionId];
    await this.#persist();
    connectorAudit("refresh", "deny", provider, {
      status: 409,
      code: "connector_reauthentication_required",
    });
    throw new ConnectorFailure(409, "connector_reauthentication_required");
  }

  async #refreshXConnector(connectionId: string, connector: StoredConnector): Promise<StoredConnector> {
    const credentials = providerCredentials("x", this.#env);
    const response = await providerFetch(buildXRefreshRequest(
      credentials.clientId,
      credentials.clientSecret,
      connector.refreshToken!,
    ));
    if (REDIRECT_STATUS.has(response.status) || !response.ok) {
      await response.body?.cancel();
      if (response.status === 400 || response.status === 401) {
        return this.#rejectRefresh("x", connectionId, connector);
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
      return this.#rejectRefresh("x", connectionId, connector);
    }
    const next: StoredConnector = {
      ...connector,
      accessToken: refreshed.accessToken,
      refreshToken: refreshed.refreshToken ?? connector.refreshToken!,
      expiresAt: Date.now() + refreshed.expiresIn * 1_000,
      scopes: [...refreshed.scopes],
    };
    this.#connections("x")[connectionId] = next;
    await this.#persist();
    connectorAudit("refresh", "allow", "x", { status: 200 });
    return next;
  }

  async #refreshGoogleConnector(
    connectionId: string,
    connector: StoredConnector,
  ): Promise<StoredConnector> {
    const credentials = providerCredentials("google", this.#env);
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
        return this.#rejectRefresh("google", connectionId, connector);
      }
      connectorAudit("refresh", "error", "google", {
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
    this.#connections("google")[connectionId] = next;
    await this.#persist();
    connectorAudit("refresh", "allow", "google", { status: 200, connection_id: connectionId });
    return next;
  }

  async #refreshSlackConnector(
    connectionId: string,
    connector: StoredConnector,
  ): Promise<StoredConnector> {
    const credentials = providerCredentials("slack", this.#env);
    const response = await providerFetch(buildSlackTokenRefreshRequest({
      ...credentials,
      refreshToken: connector.refreshToken!,
    }));
    if (!response.ok) {
      await response.body?.cancel();
      if (response.status === 400 || response.status === 401) {
        return this.#rejectRefresh("slack", connectionId, connector);
      }
      throw new ConnectorFailure(503, "connector_provider_unavailable");
    }
    let refreshed;
    try {
      refreshed = decodeSlackRefreshResponse(await providerJson(response), {
        teamId: connector.teamId!, teamName: connector.teamName!, userId: connector.userId!,
      });
    } catch {
      return this.#rejectRefresh("slack", connectionId, connector);
    }
    const next: StoredConnector = {
      ...connector,
      accessToken: refreshed.accessToken,
      refreshToken: refreshed.refreshToken!,
      expiresAt: Date.now() + refreshed.expiresIn! * 1_000,
      scopes: [...refreshed.scopes],
    };
    this.#connections("slack")[connectionId] = next;
    await this.#persist();
    connectorAudit("refresh", "allow", "slack", { status: 200, connection_id: connectionId });
    return next;
  }

  async #revoke(id: OAuthProviderId, connector: StoredConnector): Promise<boolean> {
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
    if (id === "slack") {
      const response = await providerFetch(buildSlackRevocationRequest(connector.accessToken));
      let revoked = false;
      if (response.ok) {
        try { revoked = (await providerJson(response)).ok === true; } catch { revoked = false; }
      } else {
        await response.body?.cancel();
      }
      connectorAudit("revoke", revoked ? "allow" : "error", id, { status: response.status });
      if (!revoked) throw new ConnectorFailure(503, "connector_revocation_failed");
      return true;
    }
    const response = await providerFetch(revocationRequest(id, connector, this.#env));
    await response.body?.cancel();
    const revoked = id === "github" ? response.status === 204 : response.status === 200;
    connectorAudit("revoke", revoked ? "allow" : "error", id, {
      status: response.status,
    });
    if (!revoked) throw new ConnectorFailure(503, "connector_revocation_failed");
    return true;
  }

  #connections(provider: OAuthProviderId): Record<string, StoredConnector> {
    return this.#connectors.connections[provider] ??= {};
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
      const provider = providerForCapability(id);
      const connections = Object.entries(this.#connections(provider))
        .filter(([connectionId, connector]) => CONNECTION_ID.test(connectionId)
          && this.#isUsable(connector)
          && capabilitiesFor(provider, connector.scopes).includes(id))
        .sort(([, left], [, right]) => left.connectedAt - right.connectedAt)
        .map(([connectionId, connector]) => ({
          id: connectionId,
          label: connector.label,
          account_id: connector.accountId,
          capabilities: capabilitiesFor(provider, connector.scopes),
        }));
      return { connected: connections.length > 0, connections };
    };
    return {
      github: status("github"),
      gmail: status("gmail"),
      gdrive: status("gdrive"),
      gcalendar: status("gcalendar"),
      gtasks: status("gtasks"),
      gdocs: status("gdocs"),
      gsheets: status("gsheets"),
      gslides: status("gslides"),
      gcontacts: status("gcontacts"),
      slack: status("slack"),
      x: status("x"),
    };
  }

  async #start(id: OAuthProviderId, request: Request): Promise<Record<string, unknown>> {
    const body = await readJson(request, MAX_BODY_BYTES);
    const redirectUri = stringField(body, "redirect_uri");
    const returnTo = stringField(body, "return_to");
    const accountHint = optionalAccountHint(body, id);
    if (!redirectUri || !validRedirectUri(redirectUri, this.#env)
      || !returnTo || !validReturnTo(returnTo)
      || accountHint === null) {
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
      ...(accountHint === undefined ? {} : { accountHint }),
      expiresAt: Date.now() + PENDING_TTL_MS,
    };
    await this.#persist();
    return {
      authorization_url: authorizationUrl(id, this.#env, {
        redirectUri,
        state,
        codeChallenge: challenge,
        ...(accountHint === undefined ? {} : { loginHint: accountHint }),
      }).href,
    };
  }

  async #callback(id: OAuthProviderId, request: Request): Promise<Record<string, unknown>> {
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
      if (id === "slack") {
        let token;
        try { token = decodeSlackTokenResponse(await providerJson(tokenResponse)); } catch {
          throw new ConnectorFailure(502, "connector_token_response_invalid");
        }
        const accountId = `${token.teamId}:${token.userId}`;
        const connectionId = this.#connectionIdForIdentity(id, accountId);
        const previous = this.#connections(id)[connectionId];
        const stored: StoredConnector = {
          accessToken: token.accessToken,
          ...(token.refreshToken ?? previous?.refreshToken
            ? { refreshToken: token.refreshToken ?? previous!.refreshToken }
            : {}),
          ...(token.expiresIn ? { expiresAt: Date.now() + token.expiresIn * 1_000 } : {}),
          scopes: [...token.scopes],
          accountId,
          label: boundedLabel(slackConnectionLabel(token.teamName, token.userId)),
          connectedAt: previous?.connectedAt ?? Date.now(),
          teamId: token.teamId,
          teamName: token.teamName,
          userId: token.userId,
        };
        this.#connections(id)[connectionId] = stored;
        await this.#persist();
        return { connected: true, connection_id: connectionId,
          capabilities: ["slack"], return_to: pending.returnTo };
      }
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
      const connectionId = this.#connectionIdForIdentity(id, identity.accountId);
      const previous = this.#connections(id)[connectionId];
      const connected: StoredConnector = {
        accessToken: token.accessToken,
        ...(token.refreshToken ?? previous?.refreshToken
          ? { refreshToken: token.refreshToken ?? previous!.refreshToken }
          : {}),
        ...(token.expiresIn ? { expiresAt: Date.now() + token.expiresIn * 1_000 } : {}),
        ...(token.refreshExpiresIn
          ? { refreshExpiresAt: Date.now() + token.refreshExpiresIn * 1_000 }
          : {}),
        scopes: [...token.scopes],
        accountId: identity.accountId,
        label: boundedLabel(identity.displayLabel),
        connectedAt: previous?.connectedAt ?? Date.now(),
      };
      if (pending.accountHint !== undefined
        && identity.displayLabel.toLowerCase() !== pending.accountHint.toLowerCase()) {
        this.#pendingRevocations().push({ provider: id, connector: connected, attempts: 0 });
        await this.#persist();
        await this.#schedulePendingRevocations();
        try {
          if (!await this.#revoke(id, connected)) {
            throw new ConnectorFailure(503, "connector_revocation_failed");
          }
          this.#connectors.revocations = this.#pendingRevocations().filter(
            (revocation) => revocation.connector !== connected,
          );
          await this.#persist();
          await this.#schedulePendingRevocations();
        } catch {
          connectorAudit("revoke", "error", id, {
            status: 503,
            code: "connector_mismatched_grant_revocation_failed",
          });
        }
        throw new ConnectorFailure(409, "connector_account_mismatch");
      }
      this.#connections(id)[connectionId] = connected;
      await this.#persist();
      return {
        connected: true,
        connection_id: connectionId,
        capabilities: capabilitiesFor(id, connected.scopes),
        return_to: pending.returnTo,
      };
    } catch (error) {
      const problem = connectorFailure(error);
      throw new ConnectorFailure(problem.status, problem.code, pending.returnTo);
    }
  }

  #connectionIdForIdentity(provider: OAuthProviderId, accountId: string): string {
    return Object.entries(this.#connections(provider))
      .find(([id, connector]) => CONNECTION_ID.test(id) && connector.accountId === accountId)?.[0]
      ?? randomBase64Url(32);
  }

  async #persist(): Promise<void> {
    await this.#state.storage.put(STATE_KEY, {
      envelope: await this.#vault.seal(this.#connectors),
    } satisfies StoredRow);
  }

  #pendingRevocations(): PendingRevocation[] {
    return this.#connectors.revocations ??= [];
  }

  async #retryPendingRevocations(): Promise<void> {
    const retained: PendingRevocation[] = [];
    for (const revocation of this.#pendingRevocations()) {
      try {
        if (!await this.#revoke(revocation.provider, revocation.connector)) {
          throw new ConnectorFailure(503, "connector_revocation_failed");
        }
      } catch {
        retained.push({ ...revocation, attempts: revocation.attempts + 1 });
      }
    }
    this.#connectors.revocations = retained;
    await this.#persist();
    await this.#schedulePendingRevocations();
  }

  async #schedulePendingRevocations(): Promise<void> {
    const pending = this.#pendingRevocations();
    if (pending.length === 0) {
      await this.#state.storage.deleteAlarm();
      return;
    }
    const attempts = Math.min(...pending.map((revocation) => revocation.attempts));
    const delay = Math.min(REVOCATION_RETRY_BASE_MS * 2 ** attempts, REVOCATION_RETRY_MAX_MS);
    await this.#state.storage.setAlarm(Date.now() + delay);
  }

  async #restoreDurableState(): Promise<void> {
    try {
      const row = await this.#state.storage.get<StoredRow>(STATE_KEY);
      if (!row) {
        this.#connectors = { version: 2, connections: {}, pending: {} };
        return;
      }
      const opened = await this.#vault.open<ConnectorState | LegacyConnectorState>(row.envelope);
      this.#connectors = migrateConnectorState(opened.value);
      if (opened.reseal || opened.value.version === 1) await this.#persist();
    } catch {
      this.#connectors = { version: 2, connections: {}, pending: {} };
    }
  }
}

type AuthorizationFields = {
  redirectUri: string;
  state: string;
  codeChallenge: string;
  loginHint?: string;
};
type ExchangeFields = { redirectUri: string; code: string; codeVerifier: string };
type DecodedToken = {
  accessToken: string;
  refreshToken?: string;
  expiresIn?: number;
  refreshExpiresIn?: number;
  scopes: readonly string[];
};

function authorizationUrl(
  id: OAuthProviderId,
  env: ConnectorBrokerEnv,
  fields: AuthorizationFields,
): URL {
  const clientId = providerCredentials(id, env).clientId;
  if (id === "slack") return buildSlackAuthorizationUrl({
    clientId, redirectUri: fields.redirectUri, state: fields.state,
  });
  if (id === "github") return buildGitHubAuthorizationUrl({ clientId, ...fields });
  if (id === "x") return buildXAuthorizationUrl({ clientId, ...fields });
  return buildGoogleAuthorizationUrl({ clientId, ...fields });
}

function optionalAccountHint(
  value: unknown,
  id: OAuthProviderId,
): string | null | undefined {
  if (!isRecord(value) || value.account_hint === undefined) return undefined;
  if (id !== "google"
    || typeof value.account_hint !== "string") return null;
  const hint = value.account_hint.trim().toLowerCase();
  return hint.length <= 320 && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(hint) ? hint : null;
}

function tokenExchangeRequest(
  id: OAuthProviderId,
  env: ConnectorBrokerEnv,
  fields: ExchangeFields,
): Request {
  const credentials = providerCredentials(id, env);
  if (id === "slack") return buildSlackTokenRequest({
    ...credentials, code: fields.code, redirectUri: fields.redirectUri,
  });
  if (id === "github") return buildGitHubTokenRequest({
    clientId: credentials.clientId,
    clientSecret: credentials.clientSecret,
    ...fields,
  });
  if (id === "x") return buildXTokenRequest({
    clientId: credentials.clientId,
    clientSecret: credentials.clientSecret,
    ...fields,
  });
  return buildGoogleTokenRequest({ ...credentials, ...fields });
}

function decodeToken(id: Exclude<OAuthProviderId, "slack">, value: unknown): DecodedToken {
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
  const token = decodeGoogleTokenResponse(value);
  return {
    accessToken: token.accessToken,
    ...(token.refreshToken ? { refreshToken: token.refreshToken } : {}),
    expiresIn: token.expiresIn,
    scopes: token.scopes,
  };
}

function identityRequest(id: Exclude<OAuthProviderId, "slack">, accessToken: string): Request {
  if (id === "github") return buildGitHubIdentityRequest(accessToken);
  if (id === "x") return buildXIdentityRequest(accessToken);
  return buildGoogleIdentityRequest(accessToken);
}

function revocationRequest(
  id: Exclude<OAuthProviderId, "slack" | "x">,
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

function decodeIdentity(
  id: Exclude<OAuthProviderId, "slack">,
  value: unknown,
): { accountId: string; displayLabel: string } {
  if (id === "github") return decodeGitHubIdentity(value);
  if (id === "x") return decodeXIdentity(value);
  return decodeGoogleIdentity(value);
}

function providerCredentials(
  id: OAuthProviderId,
  env: ConnectorBrokerEnv,
): { clientId: string; clientSecret: string } {
  const clientId = (id === "github" ? env.GITHUB_OAUTH_CLIENT_ID
    : id === "x" ? env.X_OAUTH_CLIENT_ID
    : id === "slack" ? env.SLACK_OAUTH_CLIENT_ID
    : env.GOOGLE_OAUTH_CLIENT_ID)?.trim();
  const clientSecret = (id === "github"
    ? env.GITHUB_OAUTH_CLIENT_SECRET
    : id === "x" ? env.X_OAUTH_CLIENT_SECRET
    : id === "slack" ? env.SLACK_OAUTH_CLIENT_SECRET
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

function capabilitiesFor(provider: OAuthProviderId, scopes: readonly string[]): ConnectorId[] {
  if (provider === "google") return googleCapabilities(scopes);
  return [provider];
}

function providerForCapability(id: ConnectorId): OAuthProviderId {
  return id.startsWith("g") && id !== "github" ? "google" : id as OAuthProviderId;
}

function oauthProviderId(value: string | undefined): OAuthProviderId | undefined {
  const normalized = value === "gmail" || value === "gdrive" ? "google" : value;
  return normalized && PROVIDER.test(normalized) ? normalized as OAuthProviderId : undefined;
}

function migrateConnectorState(value: ConnectorState | LegacyConnectorState): ConnectorState {
  if (value.version === 2) return value;
  const migrated: ConnectorState = { version: 2, connections: {}, pending: {} };
  for (const [legacyId, connector] of Object.entries(value.connectors)) {
    if (!connector) continue;
    const provider = oauthProviderId(legacyId)!;
    const connections = migrated.connections[provider] ??= {};
    const normalized = normalizeStoredConnector(connector);
    if (provider !== "google") {
      connections[randomBase64Url(32)] = normalized;
      continue;
    }
    const existing = Object.entries(connections)
      .find(([, candidate]) => candidate.accountId === normalized.accountId);
    if (!existing) {
      connections[randomBase64Url(32)] = normalized;
      continue;
    }
    connections[existing[0]] = mergeLegacyGoogleConnectors(existing[1], normalized);
  }
  for (const connector of Object.values(value.slack ?? {})) {
    (migrated.connections.slack ??= {})[randomBase64Url(32)] = normalizeStoredConnector(connector);
  }
  for (const [legacyId, pending] of Object.entries(value.pending)) {
    if (!pending) continue;
    migrated.pending[oauthProviderId(legacyId)!] = pending;
  }
  migrated.revocations = (value.revocations ?? []).map((revocation) => ({
    provider: oauthProviderId(revocation.id)!,
    connector: normalizeStoredConnector(revocation.connector),
    attempts: revocation.attempts,
  }));
  return migrated;
}

function mergeLegacyGoogleConnectors(
  left: StoredConnector,
  right: StoredConnector,
): StoredConnector {
  // Both legacy records represent one Google OAuth grant. The newest
  // incremental authorization is the only credential whose returned scope
  // list can authoritatively describe what its token may access; never infer a
  // wider token by unioning scopes from an older credential.
  return right.connectedAt >= left.connectedAt ? right : left;
}

function normalizeStoredConnector(connector: StoredConnector): StoredConnector {
  return { ...connector, label: boundedLabel(connector.label) };
}

function boundedLabel(value: string): string {
  const label = value.trim();
  if (!label) throw new ConnectorFailure(502, "connector_identity_response_invalid");
  return [...label].slice(0, 256).join("");
}

function providerRule(url: URL): ProviderRule | undefined {
  return PROVIDER_RULES.find((candidate) => candidate.origin === url.origin
    && canonicalConnectorPath(candidate.id, url.pathname)
    && candidate.paths.some((path) => path.test(url.pathname)));
}

function safeQuery(parameters: URLSearchParams): boolean {
  const forbidden = new Set(["access_token", "api_key", "authorization", "key", "oauth_token", "token"]);
  let count = 0;
  for (const [name, value] of parameters) {
    count += 1;
    if (count > 64 || name.length > 128 || value.length > 4_096
      || forbidden.has(name.toLowerCase())) return false;
  }
  return true;
}

async function slackRequestBody(request: Request): Promise<string | null> {
  if (request.body === null) return null;
  const contentType = (request.headers.get("content-type") ?? "")
    .split(";", 1)[0]!.trim().toLowerCase();
  const body = await readBoundedText(request, MAX_SLACK_REQUEST_BYTES);
  const forbidden = new Set(["access_token", "api_key", "authorization", "oauth_token", "token"]);
  if (contentType === "application/json") {
    let value: unknown;
    try { value = JSON.parse(body); } catch {
      throw new ConnectorFailure(400, "invalid_request_body");
    }
    if (!isRecord(value)) throw new ConnectorFailure(400, "invalid_request_body");
    if (Object.keys(value).some((name) => forbidden.has(name.toLowerCase()))) {
      throw new ConnectorFailure(403, "credential_input_denied");
    }
    return body;
  }
  if (contentType === "application/x-www-form-urlencoded") {
    const parameters = new URLSearchParams(body);
    for (const name of parameters.keys()) {
      if (forbidden.has(name.toLowerCase())) {
        throw new ConnectorFailure(403, "credential_input_denied");
      }
    }
    return body;
  }
  throw new ConnectorFailure(415, "unsupported_media_type");
}

function connectorRequestHeaders(
  caller: Headers,
  id: ConnectorId,
  accessToken: string | undefined,
): Headers {
  const headers = new Headers({
    accept: boundedCallerHeader(caller, "accept") ?? "application/json",
  });
  if (accessToken !== undefined) headers.set("authorization", `Bearer ${accessToken}`);
  for (const name of [
    "content-range",
    "content-type",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
    "git-protocol",
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
  connector: ConnectorId | OAuthProviderId,
  detail: Readonly<Record<string, boolean | number | string>>,
): void {
  const log = outcome === "error" ? console.error : outcome === "deny" ? console.warn : console.info;
  log({
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
