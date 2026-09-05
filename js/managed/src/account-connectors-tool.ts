import type { NamedTool } from "nanocodex";

import {
  CONNECTOR_CAPABILITY_IDS,
  CONNECTOR_PROVIDER_IDS,
  connectorConnectionId,
  connectorProviderId,
  connectorStatuses,
  projectConnectorStatus,
  type ConnectorCapabilityId,
  type ConnectorConnectionSelection,
  type ConnectorProviderId,
  type ConnectorStatus,
} from "./connector-status";

const GOOGLE_CAPABILITIES = [
  "gmail",
  "gdrive",
  "gcalendar",
  "gtasks",
  "gdocs",
  "gsheets",
  "gslides",
  "gcontacts",
] as const satisfies readonly ConnectorCapabilityId[];
const PROVIDER_CAPABILITIES: Readonly<Record<ConnectorProviderId, readonly ConnectorCapabilityId[]>> = {
  github: ["github"],
  google: GOOGLE_CAPABILITIES,
  slack: ["slack"],
  x: ["x"],
};
const CONNECTOR_NAMES: Readonly<Record<ConnectorProviderId, string>> = Object.freeze({
  github: "GitHub",
  google: "Google Workspace",
  slack: "Slack",
  x: "X",
});
const AUTHORIZATION_ENDPOINTS: Readonly<Record<ConnectorProviderId, {
  origin: string;
  pathname: string;
  pkce: boolean;
}>> = {
  github: { origin: "https://github.com", pathname: "/login/oauth/authorize", pkce: true },
  google: { origin: "https://accounts.google.com", pathname: "/o/oauth2/v2/auth", pkce: true },
  slack: { origin: "https://slack.com", pathname: "/oauth/v2/authorize", pkce: false },
  x: { origin: "https://x.com", pathname: "/i/oauth2/authorize", pkce: true },
};
const AUTHORIZATION_QUERY_KEYS = new Set([
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
const BROKER_TIMEOUT_MS = 10_000;

/** @deprecated Account connector controls now use provider IDs. */
export type AccountConnectorId = ConnectorProviderId | "gmail" | "gdrive";

type AccountConnectorsToolOptions = Readonly<{
  broker: Fetcher;
  userId: string;
  sessionId: string;
  publicOrigin: string;
  canManage(): boolean;
  allowedConnectors(): readonly ConnectorCapabilityId[] | undefined;
  allowedConnectorConnections?(): ConnectorConnectionSelection | undefined;
}>;

/** Creates the account-owned connector control tool exposed to a managed agent. */
export function accountConnectorsTool(options: AccountConnectorsToolOptions): NamedTool {
  return {
    name: "account_connectors",
    description: [
      "List, connect, reconnect, or disconnect account connectors without exposing credentials.",
      "Google Workspace is one authorization identity whose connections list the exact Gmail, Drive, Calendar, Tasks, Docs, Sheets, Slides, and Contacts capabilities granted.",
      "Connect returns a provider authorization URL. Give that exact URL to the user as a link; the provider may still require consent.",
      "Disconnect revokes one exact listed connection_id and is allowed only when the user explicitly asks to remove or replace it.",
    ].join(" "),
    supportsParallelToolCalls: false,
    parameters: {
      type: "object",
      properties: {
        operation: { type: "string", enum: ["list", "connect", "disconnect"] },
        connector: {
          type: "string",
          enum: [...CONNECTOR_PROVIDER_IDS, "gmail", "gdrive"],
          description: "OAuth provider. gmail and gdrive are accepted as legacy aliases for google.",
        },
        connection_id: {
          type: "string",
          pattern: "^[A-Za-z0-9_-]{43}$",
          description: "Exact opaque connection id returned by list; required when more than one is present.",
        },
        account_hint: {
          type: "string",
          description: "Exact email address to select for Google Workspace.",
          maxLength: 320,
        },
      },
      required: ["operation"],
      additionalProperties: false,
    },
    handler: async (input: unknown) => manageAccountConnectors(options, input),
  };
}

/** Executes one bounded connector control operation against the credential broker. */
export async function manageAccountConnectors(
  options: AccountConnectorsToolOptions,
  input: unknown,
): Promise<unknown> {
  const operation = connectorOperation(input);
  if (operation.operation === "list") {
    return {
      connectors: await visibleConnectorStatuses(options),
      supported: CONNECTOR_PROVIDER_IDS.map((id) => ({
        id,
        name: CONNECTOR_NAMES[id],
        capabilities: PROVIDER_CAPABILITIES[id],
      })),
    };
  }
  if (!options.canManage()) {
    return {
      ok: false,
      status: "forbidden",
      message: "This delegated app grant cannot change account-level connectors.",
    };
  }
  if (operation.operation === "disconnect") {
    const connectionId = operation.connectionId
      ?? await soleProviderConnectionId(options, operation.provider);
    if (!connectionId) {
      return {
        ok: false,
        status: "conflict",
        connector: operation.provider,
        message: "Choose the exact connection_id to disconnect.",
      };
    }
    const response = await brokerFetch(
      options.broker,
      `${connectorBrokerUrl(options.userId, operation.provider)}/connections/${connectionId}`,
      { method: "DELETE" },
    );
    if (!response.ok) return connectorFailure(response);
    await response.body?.cancel();
    return {
      ok: true,
      status: "disconnected",
      connector: operation.provider,
      connection_id: connectionId,
    };
  }

  const callback = new URL(
    `/v1/connectors/${operation.provider}/callback`,
    options.publicOrigin,
  );
  const returnTo = `/agent/${encodeURIComponent(options.sessionId)}`;
  const response = await brokerFetch(
    options.broker,
    connectorBrokerUrl(options.userId, operation.provider),
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        redirect_uri: callback.href,
        return_to: returnTo,
        ...(operation.accountHint === undefined ? {} : { account_hint: operation.accountHint }),
      }),
    },
  );
  if (!response.ok) return connectorFailure(response);
  const value: unknown = await response.json().catch(() => undefined);
  if (!isRecord(value) || typeof value.authorization_url !== "string") {
    return { ok: false, status: "unavailable", message: "The connector broker returned an invalid response." };
  }
  let authorization: URL;
  try { authorization = new URL(value.authorization_url); } catch {
    return { ok: false, status: "unavailable", message: "The connector broker returned an invalid authorization URL." };
  }
  if (!safeAuthorizationUrl(authorization, callback.href, operation.provider)) {
    return { ok: false, status: "unavailable", message: "The connector broker returned an unsafe authorization URL." };
  }
  return {
    ok: true,
    status: "authorization_required",
    connector: operation.provider,
    name: CONNECTOR_NAMES[operation.provider],
    ...(operation.accountHint === undefined ? {} : { account: operation.accountHint }),
    authorization_url: authorization.href,
    expires_in_seconds: 600,
    message: `Authorize ${CONNECTOR_NAMES[operation.provider]} to finish connecting it.`,
  };
}

function safeAuthorizationUrl(
  authorization: URL,
  callback: string,
  provider: ConnectorProviderId,
): boolean {
  const endpoint = AUTHORIZATION_ENDPOINTS[provider];
  if (authorization.origin !== endpoint.origin || authorization.pathname !== endpoint.pathname
    || authorization.username || authorization.password || authorization.hash
    || authorization.searchParams.get("redirect_uri") !== callback
    || !(authorization.searchParams.get("client_id") ?? "")
    || !(authorization.searchParams.get("state") ?? "")
    || !(authorization.searchParams.get(provider === "slack" ? "user_scope" : "scope") ?? "")) {
    return false;
  }
  if (endpoint.pkce && (
    authorization.searchParams.get("code_challenge_method") !== "S256"
    || !/^[A-Za-z0-9_-]{43}$/.test(authorization.searchParams.get("code_challenge") ?? "")
  )) return false;
  const responseType = authorization.searchParams.get("response_type");
  if (responseType !== null && responseType !== "code") return false;
  const granularConsent = authorization.searchParams.get("enable_granular_consent");
  if (granularConsent !== null && (provider !== "google" || granularConsent !== "true")) return false;
  const seen = new Set<string>();
  for (const key of authorization.searchParams.keys()) {
    if (!AUTHORIZATION_QUERY_KEYS.has(key) || seen.has(key)) return false;
    seen.add(key);
  }
  return true;
}

async function visibleConnectorStatuses(
  options: AccountConnectorsToolOptions,
): Promise<Record<ConnectorCapabilityId, ConnectorStatus>> {
  const response = await brokerFetch(
    options.broker,
    `https://broker.internal/users/${encodeURIComponent(options.userId)}/connectors`,
  );
  if (!response.ok) throw new Error(`connector listing failed with HTTP ${response.status}`);
  const statuses = connectorStatuses(await response.json());
  const allowedConnectors = options.allowedConnectors();
  const allowed = allowedConnectors === undefined ? undefined : new Set(allowedConnectors);
  const allowedConnections = options.allowedConnectorConnections?.();
  return Object.fromEntries(CONNECTOR_CAPABILITY_IDS.map((id) => {
    if (allowed && !allowed.has(id)) return [id, { connected: false, connections: [] }];
    const projected = projectConnectorStatus(statuses[id], allowedConnections?.[id]);
    if (allowedConnectors !== undefined && allowedConnections === undefined
      && projected.connections !== undefined) {
      return [id, {
        connected: projected.connected,
        ...(projected.account === undefined ? {} : { account: projected.account }),
      }];
    }
    return [id, projected];
  })) as Record<ConnectorCapabilityId, ConnectorStatus>;
}

async function soleProviderConnectionId(
  options: AccountConnectorsToolOptions,
  provider: ConnectorProviderId,
): Promise<string | undefined> {
  const statuses = await visibleConnectorStatuses(options);
  const ids = new Set(PROVIDER_CAPABILITIES[provider].flatMap(
    (capability) => statuses[capability].connections?.map(({ id }) => id) ?? [],
  ));
  return ids.size === 1 ? [...ids][0] : undefined;
}

function connectorOperation(input: unknown):
  | { operation: "list" }
  | { operation: "connect"; provider: ConnectorProviderId; accountHint?: string }
  | { operation: "disconnect"; provider: ConnectorProviderId; connectionId?: string } {
  if (!isRecord(input) || typeof input.operation !== "string") {
    throw new TypeError("operation must be list, connect, or disconnect");
  }
  if (input.operation === "list") {
    if (input.connector !== undefined || input.account_hint !== undefined
      || input.connection_id !== undefined) {
      throw new TypeError("list does not accept connector, connection_id, or account_hint");
    }
    return { operation: "list" };
  }
  const provider = connectorProviderId(input.connector);
  if (!provider) throw new TypeError("connector must name a supported account connector provider");
  if (input.operation === "disconnect") {
    if (input.account_hint !== undefined) throw new TypeError("disconnect does not accept account_hint");
    if (input.connection_id === undefined) return { operation: "disconnect", provider };
    const connectionId = connectorConnectionId(input.connection_id);
    if (!connectionId) throw new TypeError("connection_id must be an exact opaque connector connection id");
    return { operation: "disconnect", provider, connectionId };
  }
  if (input.operation !== "connect") {
    throw new TypeError("operation must be list, connect, or disconnect");
  }
  if (input.connection_id !== undefined) throw new TypeError("connect does not accept connection_id");
  if (input.account_hint === undefined) return { operation: "connect", provider };
  if (provider !== "google" || typeof input.account_hint !== "string") {
    throw new TypeError("account_hint is supported only for Google Workspace");
  }
  const accountHint = input.account_hint.trim().toLowerCase();
  if (accountHint.length > 320 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(accountHint)) {
    throw new TypeError("account_hint must be an email address");
  }
  return { operation: "connect", provider, accountHint };
}

async function connectorFailure(response: Response): Promise<unknown> {
  const value: unknown = await response.json().catch(() => undefined);
  const code = isRecord(value) && typeof value.error === "string"
    ? value.error
    : "connector_broker_failed";
  return {
    ok: false,
    status: response.status === 409 ? "conflict" : "unavailable",
    code,
    message: response.status === 409
      ? "The requested connector account did not match the account authorized at the provider."
      : "The connector could not be configured right now.",
  };
}

function connectorBrokerUrl(userId: string, provider: ConnectorProviderId): string {
  return `https://broker.internal/users/${encodeURIComponent(userId)}/connectors/${provider}`;
}

function brokerFetch(
  broker: Fetcher,
  input: RequestInfo | URL,
  init: RequestInit = {},
): Promise<Response> {
  return broker.fetch(input, {
    ...init,
    signal: AbortSignal.timeout(BROKER_TIMEOUT_MS),
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
