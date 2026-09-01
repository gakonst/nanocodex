import type { PromptInput } from "nanocodex";

const CONNECTOR_IDS = ["github", "gmail", "gdrive", "x"] as const;

type ConnectorId = typeof CONNECTOR_IDS[number];
type BrokerBinding = Readonly<{
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
}>;

export type AccountInfo = Readonly<{
  status: "disabled" | "ready" | "unavailable";
  authenticated: readonly ConnectorId[];
  accounts: Readonly<Partial<Record<ConnectorId, string>>>;
  connectorAccounts: Readonly<Partial<Record<ConnectorId, readonly Readonly<{ id: string; label: string }>[]>>>;
  identity: Readonly<Record<string, never>>;
  stablecoins: readonly [];
  authorizations: readonly [];
}>;

export async function accountInfo(
  binding: BrokerBinding,
  userId: string,
  enabled: boolean,
  allowedConnectors?: readonly ConnectorId[],
  allowedConnectorConnections?: Readonly<Partial<Record<ConnectorId, readonly string[]>>>,
): Promise<AccountInfo> {
  if (!enabled) return emptyInfo("disabled");
  try {
    const response = await binding.fetch(
      `https://broker.internal/users/${encodeURIComponent(userId)}/connectors`,
    );
    if (!response.ok) {
      await response.body?.cancel();
      return emptyInfo("unavailable");
    }
    const value: unknown = await response.json();
    if (!isRecord(value) || !isRecord(value.connectors)) {
      return emptyInfo("unavailable");
    }
    const connectors = value.connectors;
    const accounts: Partial<Record<ConnectorId, string>> = {};
    const connectorAccounts: Partial<Record<ConnectorId, readonly Readonly<{ id: string; label: string }>[]>> = {};
    const allowed = allowedConnectors === undefined ? undefined : new Set(allowedConnectors);
    const authenticated = CONNECTOR_IDS.filter((id) => {
      if (allowed && !allowed.has(id)) return false;
      const connector = connectors[id];
      if (!isRecord(connector) || connector.connected !== true) return false;
      if (typeof connector.label === "string" && connector.label.trim()) {
        accounts[id] = connector.label.trim();
      }
      if (Array.isArray(connector.connections)) {
        const decoded = connector.connections.map((connection) => {
          if (!isRecord(connection) || typeof connection.id !== "string"
            || !/^[A-Za-z0-9_-]{43}$/.test(connection.id)
            || typeof connection.label !== "string" || !connection.label.trim()) {
            throw new Error("invalid connector account");
          }
          return { id: connection.id, label: connection.label.trim() };
        });
        if (decoded.length > 32) throw new Error("too many connector accounts");
        connectorAccounts[id] = decoded;
        if (decoded.length === 1) accounts[id] = decoded[0]!.label;
      }
      return true;
    });
    return projectAccountInfo({
      status: "ready",
      authenticated,
      accounts,
      connectorAccounts,
      identity: {},
      stablecoins: [],
      authorizations: [],
    }, allowedConnectors, allowedConnectorConnections);
  } catch {
    return emptyInfo("unavailable");
  }
}

export function projectAccountInfo(
  info: AccountInfo,
  allowedConnectors?: readonly ConnectorId[],
  allowedConnectorConnections?: Readonly<Partial<Record<ConnectorId, readonly string[]>>>,
): AccountInfo {
  if (allowedConnectors === undefined && allowedConnectorConnections === undefined) return info;
  const allowed = new Set(allowedConnectors);
  const connectorAccounts = Object.fromEntries(Object.entries(info.connectorAccounts).flatMap(([id, connections]) => {
    if (!allowed.has(id as ConnectorId)) return [];
    const allowedIds = allowedConnectorConnections?.[id as ConnectorId];
    const filtered = allowedIds === undefined
      ? connections
      : connections.filter(({ id: connectionId }) => allowedIds.includes(connectionId));
    return filtered.length ? [[id, filtered]] : [];
  }));
  const authenticated = info.authenticated.filter((id) => allowed.has(id)
    && ((id !== "gmail" && id !== "gdrive")
      || allowedConnectorConnections === undefined
      || Object.hasOwn(connectorAccounts, id)));
  const accounts = Object.fromEntries(Object.entries(info.accounts).filter(([id]) => (
    authenticated.includes(id as ConnectorId)
  )));
  for (const id of ["gmail", "gdrive"] as const) {
    const connections = connectorAccounts[id];
    if (connections?.length === 1) accounts[id] = connections[0]!.label;
    else delete accounts[id];
  }
  return {
    ...info,
    authenticated,
    accounts,
    connectorAccounts,
  };
}

export function withInitialAccountInfo(input: PromptInput, info: AccountInfo): PromptInput {
  const explanation = [
    "The managed runtime already resolved the following non-secret accountInfo snapshot for",
    "this agent. Use it as the current connected-account context. Do not call accountInfo",
    "again unless the task requires state refreshed after this first prompt.",
  ].join(" ");
  const context = {
    type: "text" as const,
    text: `${explanation}\n\n<account_info>\n${JSON.stringify(info)}\n</account_info>`,
  };
  return typeof input === "string"
    ? [context, { type: "text", text: input }]
    : [context, ...input];
}

function emptyInfo(status: "disabled" | "unavailable"): AccountInfo {
  return {
    status,
    authenticated: [],
    accounts: {},
    connectorAccounts: {},
    identity: {},
    stablecoins: [],
    authorizations: [],
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
