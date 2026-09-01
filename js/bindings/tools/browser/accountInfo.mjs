import { namedTool } from "../namedTool.mjs";

const CONNECTOR_IDS = ["github", "gmail", "gdrive", "x", "chatgpt"];
const SLACK_CONNECTOR = /^slack:[A-Z0-9]{1,32}$/;
const CONNECTOR_SCHEMA = {
  anyOf: [
    { type: "string", enum: CONNECTOR_IDS },
    { type: "string", pattern: "^slack:[A-Z0-9]{1,32}$" },
  ],
};
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
const ACCOUNT_INFO_SCHEMA = Object.freeze({
  type: "object",
  properties: {
    status: {
      type: "string",
      enum: ["ready", "requires_login", "unavailable"],
    },
    authenticated: {
      type: "array",
      items: CONNECTOR_SCHEMA,
    },
    accounts: {
      type: "object",
      properties: Object.fromEntries(CONNECTOR_IDS.map((id) => [id, { type: "string" }])),
      patternProperties: { "^slack:[A-Z0-9]{1,32}$": { type: "string" } },
      additionalProperties: false,
    },
    identity: {
      type: "object",
      properties: { tempoAddress: { type: "string" } },
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
          connectors: { type: "array", items: CONNECTOR_SCHEMA },
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
          "accessKey",
          "spend",
        ],
        additionalProperties: false,
      },
    },
  },
  required: ["status", "authenticated", "accounts", "identity", "stablecoins", "authorizations"],
  additionalProperties: false,
});

export function browserAccountInfoTool(options) {
  return namedTool("accountInfo", {
    description: "Report account authentication, stablecoin balances, and app authorization boundaries. Never returns credentials.",
    parameters: { type: "object", additionalProperties: false },
    outputSchema: ACCOUNT_INFO_SCHEMA,
    handler: (_input, context) => browserAccountInfo(options, context?.signal),
  });
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
    const authenticated = CONNECTOR_IDS.filter((id) => {
      const connector = value.connectors[id];
      if (!record(connector) || connector.connected !== true) return false;
      if (typeof connector.label === "string" && connector.label.trim()) {
        accounts[id] = connector.label.trim();
      }
      return true;
    });
    const slack = value.connectors.slack;
    if (record(slack) && Array.isArray(slack.connections)) {
      for (const connection of slack.connections.slice(0, 64)) {
        if (!record(connection) || typeof connection.id !== "string"
          || !/^[A-Z0-9]{1,32}$/.test(connection.id)) continue;
        const reference = `slack:${connection.id}`;
        authenticated.push(reference);
        if (typeof connection.label === "string" && connection.label.trim()) {
          accounts[reference] = connection.label.trim();
        }
      }
    }
    const accountIdentity = identity(value.identity);
    const accountStablecoins = stablecoins(value.stablecoins);
    const accountAuthorizations = authorizations(value.authorizations);
    if (options.requireAuthorization && (
      !accountIdentity.tempoAddress
      || !Array.isArray(value.stablecoins)
      || accountStablecoins.length !== value.stablecoins.length
      || accountStablecoins.length === 0
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
      identity: accountIdentity,
      stablecoins: accountStablecoins,
      authorizations: accountAuthorizations,
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
    identity: {},
    stablecoins: [],
    authorizations: [],
  };
}

function identity(value) {
  if (!record(value) || typeof value.tempoAddress !== "string") return {};
  return { tempoAddress: value.tempoAddress };
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
    && value.connectors.every((connector) => CONNECTOR_IDS.includes(connector)
      || (typeof connector === "string" && SLACK_CONNECTOR.test(connector)))
    && validAccessKey(value.accessKey)
    && validSpend(value.spend);
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
