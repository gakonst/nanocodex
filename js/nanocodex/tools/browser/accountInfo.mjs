import { namedTool } from "../namedTool.mjs";

const CONNECTOR_IDS = [
  "github",
  "gmail",
  "gdrive",
  "gcalendar",
  "gtasks",
  "gdocs",
  "gsheets",
  "gslides",
  "gcontacts",
  "slack",
  "x",
  "chatgpt",
];
const ACCOUNT_CONNECTION_IDS = Object.freeze(CONNECTOR_IDS.filter((id) => id !== "chatgpt"));
const ACCOUNT_CONNECTION_LABELS = Object.freeze({
  github: "GitHub",
  gmail: "Gmail",
  gdrive: "Google Drive",
  gcalendar: "Google Calendar",
  gtasks: "Google Tasks",
  gdocs: "Google Docs",
  gsheets: "Google Sheets",
  gslides: "Google Slides",
  gcontacts: "Google Contacts",
  slack: "Slack",
  x: "X",
});
const GOOGLE_CONNECTION_IDS = new Set([
  "gmail",
  "gdrive",
  "gcalendar",
  "gtasks",
  "gdocs",
  "gsheets",
  "gslides",
  "gcontacts",
]);
const ACCOUNT_AUTHORIZATION_ENDPOINTS = Object.freeze({
  github: { origin: "https://github.com", pathname: "/login/oauth/authorize" },
  google: { origin: "https://accounts.google.com", pathname: "/o/oauth2/v2/auth" },
  slack: { origin: "https://slack.com", pathname: "/oauth/v2/authorize" },
  x: { origin: "https://x.com", pathname: "/i/oauth2/authorize" },
});
const ACCOUNT_AUTHORIZATION_QUERY_KEYS = new Set([
  "access_type",
  "client_id",
  "code_challenge",
  "code_challenge_method",
  "enable_granular_consent",
  "include_granted_scopes",
  "login_hint",
  "prompt",
  "redirect_uri",
  "response_type",
  "scope",
  "state",
  "team",
  "user_scope",
]);
const LOCAL_OAUTH_RELAY_ORIGIN = "http://127.0.0.1:47891";
const CONNECTOR_CONNECTION_IDS = CONNECTOR_IDS.filter((id) => id !== "chatgpt");
const CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const CONNECTOR_CONNECTION_SCHEMA = {
  type: "object",
  properties: {
    id: { type: "string", pattern: "^[A-Za-z0-9_-]{43}$" },
    label: { type: "string" },
    accountId: { type: "string" },
    capabilities: { type: "array", items: { type: "string", enum: CONNECTOR_IDS } },
  },
  required: ["id", "label"],
  additionalProperties: false,
};
const HOST_PRINCIPAL_ID = /^[A-Za-z0-9_-]{43}$/;
const MAX_VAULT_ENTRIES = 100;
const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;
const VAULT_ID_SCHEMA = { type: "string", pattern: "^[A-Za-z0-9_-]{22,64}$" };
const vaultTextSchema = (maxLength) => ({ type: "string", minLength: 1, maxLength });
const LIMIT_SCHEMA = {
  type: "object",
  properties: {
    token: { type: "string" },
    symbol: { type: "string" },
    limit: { type: "string" },
    period: { type: "integer" },
  },
  required: ["token", "symbol", "limit"],
  additionalProperties: false,
};
const VAULT_ENTRY_SCHEMA = {
  oneOf: [
    {
      type: "object",
      properties: {
        id: VAULT_ID_SCHEMA,
        kind: { type: "string", enum: ["login"] },
        name: vaultTextSchema(120),
        created_at: { type: "integer", minimum: 0 },
        username: vaultTextSchema(512),
      },
      required: ["id", "kind", "name", "created_at", "username"],
      additionalProperties: false,
    },
    {
      type: "object",
      properties: {
        id: VAULT_ID_SCHEMA,
        kind: { type: "string", enum: ["card"] },
        name: vaultTextSchema(120),
        created_at: { type: "integer", minimum: 0 },
        last4: { type: "string", pattern: "^[0-9]{4}$" },
      },
      required: ["id", "kind", "name", "created_at", "last4"],
      additionalProperties: false,
    },
    {
      type: "object",
      properties: {
        id: VAULT_ID_SCHEMA,
        kind: { type: "string", enum: ["address"] },
        name: vaultTextSchema(120),
        created_at: { type: "integer", minimum: 0 },
        address_line_1: vaultTextSchema(256),
        address_line_2: vaultTextSchema(256),
        city: vaultTextSchema(120),
        state: vaultTextSchema(120),
        zip: vaultTextSchema(32),
        country: vaultTextSchema(120),
      },
      required: ["id", "kind", "name", "created_at", "address_line_1", "city", "state", "zip", "country"],
      additionalProperties: false,
    },
    {
      type: "object",
      properties: {
        id: VAULT_ID_SCHEMA,
        kind: { type: "string", enum: ["phone"] },
        name: vaultTextSchema(120),
        created_at: { type: "integer", minimum: 0 },
        phone_number: vaultTextSchema(64),
      },
      required: ["id", "kind", "name", "created_at", "phone_number"],
      additionalProperties: false,
    },
  ],
};
const ACCOUNT_INFO_SCHEMA = Object.freeze({
  type: "object",
  properties: {
    status: {
      type: "string",
      enum: ["ready", "requires_login", "unavailable"],
    },
    authenticated: {
      type: "array",
      items: { type: "string", enum: CONNECTOR_IDS },
    },
    accounts: {
      type: "object",
      properties: Object.fromEntries(CONNECTOR_IDS.map((id) => [id, { type: "string" }])),
      additionalProperties: false,
    },
    connectorAccounts: {
      type: "object",
      properties: Object.fromEntries(CONNECTOR_IDS.map((id) => [id, {
        type: "array",
        items: CONNECTOR_CONNECTION_SCHEMA,
      }])),
      additionalProperties: false,
    },
    identity: {
      type: "object",
      properties: {
        tempoAddress: { type: "string" },
        hostPrincipal: {
          type: "object",
          properties: {
            kind: { type: "string", enum: ["host"] },
            id: { type: "string" },
          },
          required: ["kind", "id"],
          additionalProperties: false,
        },
      },
      additionalProperties: false,
    },
    stablecoins: {
      type: "array",
      items: {
        type: "object",
        properties: {
          token: { type: "string" },
          symbol: { type: "string" },
          balance: { type: "string" },
          decimals: { type: "integer" },
        },
        required: ["token", "symbol", "balance", "decimals"],
        additionalProperties: false,
      },
    },
    authorizations: {
      type: "array",
      items: {
        type: "object",
        properties: {
          appId: { type: "string" },
          permission: { type: "string" },
          status: { type: "string", enum: ["active", "revoked", "expired"] },
          expiresAt: { type: "integer" },
          capabilities: { type: "array", items: { type: "string" } },
          connectors: { type: "array", items: { type: "string", enum: CONNECTOR_IDS } },
          connectorConnections: {
            type: "object",
            properties: Object.fromEntries(CONNECTOR_CONNECTION_IDS.map((id) => [id, {
              type: "array",
              maxItems: 64,
              uniqueItems: true,
              items: { type: "string", pattern: "^[A-Za-z0-9_-]{43}$" },
            }])),
            additionalProperties: false,
          },
          authority: { type: "string", enum: ["hosted"] },
          accessKey: {
            type: "object",
            properties: {
              id: { type: "string" },
              expiry: { type: "integer" },
              limits: { type: "array", items: LIMIT_SCHEMA },
              scopes: {
                type: "array",
                items: {
                  type: "object",
                  properties: {
                    address: { type: "string" },
                    selector: { type: "string" },
                    recipients: { type: "array", items: { type: "string" } },
                  },
                  required: ["address"],
                  additionalProperties: false,
                },
              },
            },
            required: ["id", "expiry", "limits", "scopes"],
            additionalProperties: false,
          },
          spend: {
            type: "object",
            properties: {
              token: { type: "string" },
              symbol: { type: "string" },
              spent: { type: "string" },
              limit: { type: "string" },
              period: { type: "integer" },
              maxPerRequest: { type: "string" },
            },
            required: ["token", "symbol", "spent", "limit", "period", "maxPerRequest"],
            additionalProperties: false,
          },
        },
        required: [
          "appId",
          "permission",
          "status",
          "expiresAt",
          "capabilities",
          "connectors",
        ],
        oneOf: [
          { required: ["authority"] },
          { required: ["accessKey", "spend"] },
        ],
        additionalProperties: false,
      },
    },
    vault: {
      type: "array",
      maxItems: MAX_VAULT_ENTRIES,
      items: VAULT_ENTRY_SCHEMA,
    },
  },
  required: [
    "status", "authenticated", "accounts", "connectorAccounts", "identity",
    "stablecoins", "authorizations", "vault",
  ],
  additionalProperties: false,
});

const ACCOUNT_CONNECTION_REQUEST_SCHEMA = Object.freeze({
  type: "object",
  properties: {
    status: { type: "string", enum: ["authorization_required"] },
    connector: { type: "string", enum: ACCOUNT_CONNECTION_IDS },
    label: { type: "string" },
    authorization_url: { type: "string" },
    expires_in_seconds: { type: "integer", minimum: 1 },
    message: { type: "string" },
  },
  required: [
    "status",
    "connector",
    "label",
    "authorization_url",
    "expires_in_seconds",
    "message",
  ],
  additionalProperties: false,
});

export function browserAccountInfoTool(options) {
  return namedTool("accountInfo", {
    description: "Report account authentication, safe Vault references, stablecoin balances, and app authorization boundaries. Vault references never include passwords, full card numbers, CVVs, expiry details, or billing ZIPs.",
    parameters: { type: "object", additionalProperties: false },
    outputSchema: ACCOUNT_INFO_SCHEMA,
    handler: (_input, context) => browserAccountInfo(options, context?.signal),
  });
}

export function browserAccountConnectionTool(options) {
  return namedTool("requestAccountConnection", {
    description: "Request an account authorization link for GitHub, Gmail or another Google Workspace app, Slack, or X. Call this when the user asks to connect or authenticate one of these services. Return the exact authorization_url as a Markdown link in your response; do not claim the account is connected until accountInfo confirms it.",
    parameters: {
      type: "object",
      properties: {
        connector: { type: "string", enum: ACCOUNT_CONNECTION_IDS },
      },
      required: ["connector"],
      additionalProperties: false,
    },
    outputSchema: ACCOUNT_CONNECTION_REQUEST_SCHEMA,
    async handler(input, context) {
      const connector = input?.connector;
      if (!ACCOUNT_CONNECTION_IDS.includes(connector)) {
        throw new TypeError("account connection connector is invalid");
      }
      const label = ACCOUNT_CONNECTION_LABELS[connector];
      if (!label) throw new TypeError("account connection connector is invalid");
      if (typeof options?.fetch !== "function") {
        throw new TypeError("browser account connection requires fetch");
      }
      const provider = accountConnectionProvider(connector);
      const endpoint = new URL(`/v1/connectors/${provider}`, options.origin);
      const response = await options.fetch(endpoint, {
        method: "POST",
        headers: {
          accept: "application/json",
          "content-type": "application/json",
        },
        body: JSON.stringify({ return_to: "/connect" }),
        credentials: "same-origin",
        cache: "no-store",
        signal: context?.signal,
      });
      if (!response.ok) {
        await response.body?.cancel();
        throw new Error(`account connection request failed with HTTP ${response.status}`);
      }
      const value = await response.json().catch(() => undefined);
      const authorizationUrl = record(value) && typeof value.authorization_url === "string"
        ? safeAccountAuthorizationUrl(value.authorization_url, provider, options.origin)
        : undefined;
      if (!authorizationUrl) {
        throw new Error("account connection returned an invalid authorization URL");
      }
      return {
        status: "authorization_required",
        connector,
        label,
        authorization_url: authorizationUrl,
        expires_in_seconds: 600,
        message: `Open the authorization link to connect ${label}.`,
      };
    },
  });
}

function accountConnectionProvider(connector) {
  return GOOGLE_CONNECTION_IDS.has(connector) ? "google" : connector;
}

function safeAccountAuthorizationUrl(value, provider, publicOrigin) {
  let authorization;
  try { authorization = new URL(value); } catch { return undefined; }
  const expected = ACCOUNT_AUTHORIZATION_ENDPOINTS[provider];
  if (!expected
    || authorization.origin !== expected.origin
    || authorization.pathname !== expected.pathname
    || authorization.username
    || authorization.password
    || authorization.hash
    || !authorization.searchParams.get("client_id")
    || !authorization.searchParams.get("state")
    || !authorization.searchParams.get(provider === "slack" ? "user_scope" : "scope")
    || !safeAccountRedirectUri(authorization.searchParams.get("redirect_uri"), provider, publicOrigin)) {
    return undefined;
  }
  const seen = new Set();
  for (const key of authorization.searchParams.keys()) {
    if (!ACCOUNT_AUTHORIZATION_QUERY_KEYS.has(key) || seen.has(key)) return undefined;
    seen.add(key);
  }
  const responseType = authorization.searchParams.get("response_type");
  if (responseType !== null && responseType !== "code") return undefined;
  if (provider !== "slack" && (
    authorization.searchParams.get("code_challenge_method") !== "S256"
    || !/^[A-Za-z0-9_-]{43}$/.test(authorization.searchParams.get("code_challenge") ?? "")
  )) return undefined;
  const granularConsent = authorization.searchParams.get("enable_granular_consent");
  if (granularConsent !== null && (provider !== "google" || granularConsent !== "true")) {
    return undefined;
  }
  return authorization.href;
}

function safeAccountRedirectUri(value, provider, publicOrigin) {
  let redirect;
  try { redirect = new URL(value); } catch { return false; }
  if (redirect.username || redirect.password || redirect.search || redirect.hash
    || redirect.pathname !== `/v1/connectors/${provider}/callback`) return false;
  const origin = new URL(publicOrigin);
  if (redirect.origin === origin.origin) return true;
  return redirect.origin === LOCAL_OAUTH_RELAY_ORIGIN
    && (origin.hostname === "nanocodex.localhost"
      || origin.hostname.endsWith(".nanocodex.localhost"));
}

export function browserRuntimeInfoTool(options, descriptor) {
  if (!descriptor || typeof descriptor !== "object" || Array.isArray(descriptor)) {
    throw new TypeError("browser runtime info requires a shell descriptor");
  }
  return namedTool("runtimeInfo", {
    description: "Return information about the browser agent runtime and connected accounts.",
    parameters: { type: "object", additionalProperties: false },
    async handler(_input, context) {
      return {
        runtime: "browser-worker",
        shell: descriptor.shell,
        shell_network: descriptor.network.mode,
        sandbox: "browser",
        workspace: descriptor.cwd,
        commands: descriptor.commands,
        custom_commands: descriptor.customCommands,
        limits: descriptor.limits,
        pty: descriptor.pty,
        sessions: descriptor.sessions,
        sandbox_escalation: descriptor.sandboxEscalation,
        account: await browserAccountInfo(options, context?.signal),
      };
    },
  });
}

export async function browserAccountInfo(options, signal) {
  if (typeof options?.fetch !== "function") throw new TypeError("browser account info requires fetch");
  const endpoint = new URL(options.endpoint ?? "/v1/connectors", options.origin);
  try {
    const response = await options.fetch(endpoint, {
      headers: { accept: "application/json" },
      credentials: "same-origin",
      cache: "no-store",
      signal,
    });
    if (response.status === 401) {
      await response.body?.cancel();
      return emptyInfo("requires_login");
    }
    if (!response.ok) {
      await response.body?.cancel();
      return emptyInfo("unavailable");
    }
    const value = await response.json();
    if (!record(value) || !record(value.connectors)) return emptyInfo("unavailable");
    const accounts = {};
    const connectorAccounts = {};
    const authenticated = CONNECTOR_IDS.filter((id) => {
      const connector = value.connectors[id];
      if (!record(connector) || connector.connected !== true) return false;
      if (Array.isArray(connector.connections)) {
        if (connector.connections.length > 64) throw new Error("too many connector accounts");
        const connections = connector.connections.map(connectorAccount);
        if (new Set(connections.map(({ id: connectionId }) => connectionId)).size
          !== connections.length) throw new Error("duplicate connector accounts");
        connectorAccounts[id] = connections;
        if (connections.length === 1) accounts[id] = connections[0].label;
        return connections.length > 0;
      }
      // Backward-compatible legacy singleton status.
      const legacyLabel = boundedString(connector.label, 256)
        ?? boundedString(connector.account_id, 256);
      if (legacyLabel) accounts[id] = legacyLabel;
      return true;
    });
    const accountIdentity = identity(value.identity);
    const accountStablecoins = stablecoins(value.stablecoins);
    const accountAuthorizations = authorizations(value.authorizations);
    const accountVault = vaultEntries(value.vault);
    if (options.requireAuthorization && (
      (!accountIdentity.tempoAddress && !accountIdentity.hostPrincipal)
      || !Array.isArray(value.stablecoins)
      || accountStablecoins.length !== value.stablecoins.length
      || (accountIdentity.tempoAddress && accountStablecoins.length === 0)
      || !Array.isArray(value.authorizations)
      || accountAuthorizations.length !== value.authorizations.length
      || accountAuthorizations.length === 0
    )) {
      throw new Error("account authorization response is incomplete");
    }
    return {
      status: "ready",
      authenticated,
      accounts,
      connectorAccounts,
      identity: accountIdentity,
      stablecoins: accountStablecoins,
      authorizations: accountAuthorizations,
      vault: accountVault,
    };
  } catch (error) {
    if (signal?.aborted) throw error;
    return emptyInfo("unavailable");
  }
}

function emptyInfo(status) {
  return {
    status,
    authenticated: [],
    accounts: {},
    connectorAccounts: {},
    identity: {},
    stablecoins: [],
    authorizations: [],
    vault: [],
  };
}

function connectorAccount(value) {
  if (!record(value) || typeof value.id !== "string" || !CONNECTION_ID.test(value.id)) {
    throw new Error("invalid connector account");
  }
  const label = boundedString(value.label, 256);
  const accountId = value.account_id === undefined
    ? undefined
    : boundedString(value.account_id, 256);
  const capabilities = value.capabilities === undefined
    ? undefined
    : connectorCapabilities(value.capabilities);
  if (!label || (value.account_id !== undefined && !accountId)) {
    throw new Error("invalid connector account");
  }
  return {
    id: value.id,
    label,
    ...(accountId === undefined ? {} : { accountId }),
    ...(capabilities === undefined ? {} : { capabilities }),
  };
}

function connectorCapabilities(value) {
  if (!Array.isArray(value) || value.length > CONNECTOR_IDS.length
    || value.some((capability) => !CONNECTOR_IDS.includes(capability))
    || new Set(value).size !== value.length) {
    throw new Error("invalid connector capabilities");
  }
  return [...value];
}

function boundedString(value, maxLength) {
  if (typeof value !== "string") return undefined;
  const normalized = value.trim();
  return normalized && normalized.length <= maxLength ? normalized : undefined;
}

function vaultEntries(value) {
  if (!Array.isArray(value) || value.length > MAX_VAULT_ENTRIES) return [];
  const projected = [];
  for (const entry of value) {
    const safe = vaultEntry(entry);
    if (!safe) return [];
    projected.push(safe);
  }
  return projected;
}

function vaultEntry(value) {
  if (!record(value)
    || typeof value.id !== "string" || !VAULT_ID.test(value.id)
    || !vaultText(value.name, 120)
    || !Number.isSafeInteger(value.created_at)
    || value.created_at < 0) return undefined;
  const common = { id: value.id, name: value.name, created_at: value.created_at };
  if (value.kind === "login"
    && exactKeys(value, ["id", "kind", "name", "created_at", "username"])
    && vaultText(value.username, 512)) {
    return { ...common, kind: "login", username: value.username };
  }
  if (value.kind === "card"
    && exactKeys(value, ["id", "kind", "name", "created_at", "last4"])
    && typeof value.last4 === "string" && /^[0-9]{4}$/.test(value.last4)) {
    return { ...common, kind: "card", last4: value.last4 };
  }
  if (value.kind === "address") {
    const hasLine2 = Object.prototype.hasOwnProperty.call(value, "address_line_2");
    if (!exactKeys(value, [
      "id", "kind", "name", "created_at", "address_line_1",
      ...(hasLine2 ? ["address_line_2"] : []),
      "city", "state", "zip", "country",
    ])
      || !vaultText(value.address_line_1, 256)
      || (hasLine2 && !vaultText(value.address_line_2, 256))
      || !vaultText(value.city, 120)
      || !vaultText(value.state, 120)
      || !vaultText(value.zip, 32)
      || !vaultText(value.country, 120)) return undefined;
    return {
      ...common,
      kind: "address",
      address_line_1: value.address_line_1,
      ...(hasLine2 ? { address_line_2: value.address_line_2 } : {}),
      city: value.city,
      state: value.state,
      zip: value.zip,
      country: value.country,
    };
  }
  if (value.kind === "phone"
    && exactKeys(value, ["id", "kind", "name", "created_at", "phone_number"])
    && vaultText(value.phone_number, 64)) {
    return { ...common, kind: "phone", phone_number: value.phone_number };
  }
  return undefined;
}

function exactKeys(value, expected) {
  const keys = Object.keys(value);
  return keys.length === expected.length && keys.every((key) => expected.includes(key));
}

function vaultText(value, maxBytes) {
  return typeof value === "string" && value.length > 0 && value.trim() === value
    && !/[\u0000-\u001f\u007f]/.test(value)
    && new TextEncoder().encode(value).byteLength <= maxBytes;
}

function identity(value) {
  if (!record(value)) return {};
  const hasTempoAddress = typeof value.tempoAddress === "string";
  const hasHostPrincipal = validHostPrincipal(value.hostPrincipal);
  if (hasTempoAddress === hasHostPrincipal) return {};
  if (hasTempoAddress) return { tempoAddress: value.tempoAddress };
  if (hasHostPrincipal) {
    return { hostPrincipal: { kind: "host", id: value.hostPrincipal.id } };
  }
  return {};
}

function stablecoins(value) {
  if (!Array.isArray(value)) return [];
  return value.filter((asset) => record(asset)
    && typeof asset.token === "string"
    && typeof asset.symbol === "string"
    && typeof asset.balance === "string"
    && Number.isSafeInteger(asset.decimals)
  ).map(({ token, symbol, balance, decimals }) => ({ token, symbol, balance, decimals }));
}

function authorizations(value) {
  if (!Array.isArray(value)) return [];
  return value.filter(validAuthorization).map((authorization) => ({
    appId: authorization.appId,
    permission: authorization.permission,
    status: authorization.status,
    expiresAt: authorization.expiresAt,
    capabilities: [...authorization.capabilities],
    connectors: [...authorization.connectors],
    ...(authorization.connectorConnections === undefined ? {} : {
      connectorConnections: Object.fromEntries(Object.entries(
        authorization.connectorConnections,
      ).map(([connector, ids]) => [connector, [...ids]])),
    }),
    ...(authorization.authority === "hosted" ? { authority: "hosted" } : {
      accessKey: {
        id: authorization.accessKey.id,
        expiry: authorization.accessKey.expiry,
        limits: authorization.accessKey.limits.map((limit) => ({
          token: limit.token,
          symbol: limit.symbol,
          limit: limit.limit,
          ...(limit.period === undefined ? {} : { period: limit.period }),
        })),
        scopes: authorization.accessKey.scopes.map((scope) => ({
          address: scope.address,
          ...(scope.selector === undefined ? {} : { selector: scope.selector }),
          ...(scope.recipients === undefined ? {} : { recipients: [...scope.recipients] }),
        })),
      },
      spend: {
        token: authorization.spend.token,
        symbol: authorization.spend.symbol,
        spent: authorization.spend.spent,
        limit: authorization.spend.limit,
        period: authorization.spend.period,
        maxPerRequest: authorization.spend.maxPerRequest,
      },
    }),
  }));
}

function validAuthorization(value) {
  return record(value)
    && typeof value.appId === "string"
    && typeof value.permission === "string"
    && ["active", "revoked", "expired"].includes(value.status)
    && Number.isSafeInteger(value.expiresAt)
    && stringArray(value.capabilities)
    && Array.isArray(value.connectors)
    && value.connectors.every((connector) => CONNECTOR_IDS.includes(connector))
    && validConnectorConnections(value.connectorConnections, value.connectors)
    && ((value.authority === "hosted"
      && value.accessKey === undefined
      && value.spend === undefined)
      || (value.authority === undefined
        && validAccessKey(value.accessKey)
        && validSpend(value.spend)));
}

function validHostPrincipal(value) {
  return record(value)
    && value.kind === "host"
    && typeof value.id === "string"
    && HOST_PRINCIPAL_ID.test(value.id)
    && Object.keys(value).every((key) => key === "kind" || key === "id");
}

function validConnectorConnections(value, granted) {
  if (value === undefined) return true;
  if (!record(value)) return false;
  return Object.entries(value).every(([connector, ids]) => (
    CONNECTOR_CONNECTION_IDS.includes(connector)
    && granted.includes(connector)
    && Array.isArray(ids)
    && ids.length <= 64
    && ids.every((id) => typeof id === "string" && CONNECTION_ID.test(id))
    && new Set(ids).size === ids.length
  ));
}

function validAccessKey(value) {
  return record(value)
    && typeof value.id === "string"
    && Number.isSafeInteger(value.expiry)
    && Array.isArray(value.limits)
    && value.limits.every((limit) => record(limit)
      && typeof limit.token === "string"
      && typeof limit.symbol === "string"
      && typeof limit.limit === "string"
      && (limit.period === undefined || Number.isSafeInteger(limit.period)))
    && Array.isArray(value.scopes)
    && value.scopes.every((scope) => record(scope)
      && typeof scope.address === "string"
      && (scope.selector === undefined || typeof scope.selector === "string")
      && (scope.recipients === undefined || stringArray(scope.recipients)));
}

function validSpend(value) {
  return record(value)
    && typeof value.token === "string"
    && typeof value.symbol === "string"
    && typeof value.spent === "string"
    && typeof value.limit === "string"
    && Number.isSafeInteger(value.period)
    && typeof value.maxPerRequest === "string";
}

function stringArray(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function record(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
