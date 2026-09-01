import type { NamedTool } from "nanocodex";

const CONNECTOR_IDS = ["github", "gmail", "gdrive", "x"] as const;
const CONNECTOR_NAMES = Object.freeze({
  github: "GitHub",
  gmail: "Gmail",
  gdrive: "Google Drive",
  x: "X",
});
const AUTHORIZATION_ENDPOINTS: Readonly<Record<AccountConnectorId, {
  origin: string;
  pathname: string;
}>> = {
  github: { origin: "https://github.com", pathname: "/login/oauth/authorize" },
  gmail: { origin: "https://accounts.google.com", pathname: "/o/oauth2/v2/auth" },
  gdrive: { origin: "https://accounts.google.com", pathname: "/o/oauth2/v2/auth" },
  x: { origin: "https://x.com", pathname: "/i/oauth2/authorize" },
};
const AUTHORIZATION_QUERY_KEYS = new Set([
  "access_type",
  "client_id",
  "code_challenge",
  "code_challenge_method",
  "include_granted_scopes",
  "login_hint",
  "prompt",
  "redirect_uri",
  "response_type",
  "scope",
  "state",
]);
const BROKER_TIMEOUT_MS = 10_000;

export type AccountConnectorId = typeof CONNECTOR_IDS[number];

type ConnectorStatus = Readonly<{
  connected: boolean;
  account?: string;
}>;

type AccountConnectorsToolOptions = Readonly<{
  broker: Fetcher;
  userId: string;
  sessionId: string;
  publicOrigin: string;
  canManage(): boolean;
  allowedConnectors(): readonly AccountConnectorId[] | undefined;
}>;

/** Creates the account-owned connector control tool exposed to a managed agent. */
export function accountConnectorsTool(options: AccountConnectorsToolOptions): NamedTool {
  return {
    name: "account_connectors",
    description: [
      "List, connect, reconnect, or disconnect account connectors without exposing credentials.",
      "Use connector=gmail when the user asks to connect an email address, and pass that address as account_hint.",
      "Connect returns a provider authorization URL. Give that exact URL to the user as a link; the provider may still require consent.",
      "Only disconnect when the user explicitly asks to remove or replace a connection.",
    ].join(" "),
    supportsParallelToolCalls: false,
    parameters: {
      type: "object",
      properties: {
        operation: { type: "string", enum: ["list", "connect", "disconnect"] },
        connector: { type: "string", enum: [...CONNECTOR_IDS] },
        account_hint: {
          type: "string",
          description: "Exact email address to select for Gmail or Google Drive.",
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
      connectors: await connectorStatuses(options),
      supported: CONNECTOR_IDS.map((id) => ({ id, name: CONNECTOR_NAMES[id] })),
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
    const response = await brokerFetch(
      options.broker,
      connectorBrokerUrl(options.userId, operation.connector),
      { method: "DELETE" },
    );
    if (!response.ok) return connectorFailure(response);
    await response.body?.cancel();
    return {
      ok: true,
      status: "disconnected",
      connector: operation.connector,
    };
  }

  const callback = new URL(
    `/v1/connectors/${operation.connector}/callback`,
    options.publicOrigin,
  );
  const returnTo = `/agent?${new URLSearchParams({ thread: options.sessionId })}`;
  const response = await brokerFetch(
    options.broker,
    connectorBrokerUrl(options.userId, operation.connector),
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
  if (!safeAuthorizationUrl(authorization, callback.href, operation.connector)) {
    return { ok: false, status: "unavailable", message: "The connector broker returned an unsafe authorization URL." };
  }
  return {
    ok: true,
    status: "authorization_required",
    connector: operation.connector,
    name: CONNECTOR_NAMES[operation.connector],
    ...(operation.accountHint === undefined ? {} : { account: operation.accountHint }),
    authorization_url: authorization.href,
    expires_in_seconds: 600,
    message: `Authorize ${CONNECTOR_NAMES[operation.connector]} to finish connecting it.`,
  };
}

function safeAuthorizationUrl(
  authorization: URL,
  callback: string,
  connector: AccountConnectorId,
): boolean {
  const endpoint = AUTHORIZATION_ENDPOINTS[connector];
  if (authorization.origin !== endpoint.origin || authorization.pathname !== endpoint.pathname
    || authorization.username || authorization.password || authorization.hash
    || authorization.searchParams.get("redirect_uri") !== callback
    || authorization.searchParams.get("code_challenge_method") !== "S256"
    || !/^[A-Za-z0-9_-]{43}$/.test(authorization.searchParams.get("code_challenge") ?? "")
    || !(authorization.searchParams.get("client_id") ?? "")
    || !(authorization.searchParams.get("scope") ?? "")
    || !(authorization.searchParams.get("state") ?? "")) return false;
  const responseType = authorization.searchParams.get("response_type");
  if (responseType !== null && responseType !== "code") return false;
  const seen = new Set<string>();
  for (const key of authorization.searchParams.keys()) {
    if (!AUTHORIZATION_QUERY_KEYS.has(key) || seen.has(key)) return false;
    seen.add(key);
  }
  return true;
}

async function connectorStatuses(
  options: AccountConnectorsToolOptions,
): Promise<Record<AccountConnectorId, ConnectorStatus>> {
  const response = await brokerFetch(
    options.broker,
    `https://broker.internal/users/${encodeURIComponent(options.userId)}/connectors`,
  );
  if (!response.ok) throw new Error(`connector listing failed with HTTP ${response.status}`);
  const value: unknown = await response.json();
  if (!isRecord(value) || !isRecord(value.connectors)) {
    throw new Error("connector listing returned an invalid response");
  }
  const connectorValues = value.connectors;
  const allowed = options.allowedConnectors();
  const visible = allowed === undefined ? new Set(CONNECTOR_IDS) : new Set(allowed);
  return Object.fromEntries(CONNECTOR_IDS.map((id) => {
    const status = connectorValues[id];
    const connected = visible.has(id) && isRecord(status) && status.connected === true;
    const account = connected && typeof status.label === "string" && status.label.trim()
      ? status.label.trim()
      : undefined;
    return [id, {
      connected,
      ...(account === undefined ? {} : { account }),
    }];
  })) as Record<AccountConnectorId, ConnectorStatus>;
}

function connectorOperation(input: unknown):
  | { operation: "list" }
  | { operation: "connect"; connector: AccountConnectorId; accountHint?: string }
  | { operation: "disconnect"; connector: AccountConnectorId } {
  if (!isRecord(input) || typeof input.operation !== "string") {
    throw new TypeError("operation must be list, connect, or disconnect");
  }
  if (input.operation === "list") {
    if (input.connector !== undefined || input.account_hint !== undefined) {
      throw new TypeError("list does not accept connector or account_hint");
    }
    return { operation: "list" };
  }
  const connector = CONNECTOR_IDS.find((id) => id === input.connector);
  if (!connector) throw new TypeError("connector must name a supported account connector");
  if (input.operation === "disconnect") {
    if (input.account_hint !== undefined) throw new TypeError("disconnect does not accept account_hint");
    return { operation: "disconnect", connector };
  }
  if (input.operation !== "connect") {
    throw new TypeError("operation must be list, connect, or disconnect");
  }
  if (input.account_hint === undefined) return { operation: "connect", connector };
  if ((connector !== "gmail" && connector !== "gdrive")
    || typeof input.account_hint !== "string") {
    throw new TypeError("account_hint is supported only for Gmail and Google Drive");
  }
  const accountHint = input.account_hint.trim().toLowerCase();
  if (accountHint.length > 320 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(accountHint)) {
    throw new TypeError("account_hint must be an email address");
  }
  return { operation: "connect", connector, accountHint };
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

function connectorBrokerUrl(userId: string, connector: AccountConnectorId): string {
  return `https://broker.internal/users/${encodeURIComponent(userId)}/connectors/${connector}`;
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
