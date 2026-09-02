import type { PromptInput } from "nanocodex";

import {
  CONNECTOR_CAPABILITY_IDS,
  connectorStatuses,
  projectConnectorStatus,
  type ConnectorCapabilityId,
  type ConnectorConnection,
  type ConnectorConnectionSelection,
} from "./connector-status";

type BrokerBinding = Readonly<{
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
}>;

export type AccountInfo = Readonly<{
  status: "disabled" | "ready" | "unavailable";
  /** Legacy capability-level summary retained for existing agents. */
  authenticated: readonly ConnectorCapabilityId[];
  /** Legacy single-account labels retained when a capability has one visible account. */
  accounts: Readonly<Partial<Record<ConnectorCapabilityId, string>>>;
  /** Provider-neutral, selectable connection metadata keyed by service capability. */
  connectorAccounts: Readonly<
    Partial<Record<ConnectorCapabilityId, readonly ConnectorConnection[]>>
  >;
  identity: Readonly<Record<string, never>>;
  stablecoins: readonly [];
  authorizations: readonly [];
}>;

export async function accountInfo(
  binding: BrokerBinding,
  userId: string,
  enabled: boolean,
  allowedConnectors?: readonly ConnectorCapabilityId[],
  allowedConnections?: ConnectorConnectionSelection,
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
    const statuses = connectorStatuses(await response.json());
    const allowed = allowedConnectors === undefined ? undefined : new Set(allowedConnectors);
    const authenticated: ConnectorCapabilityId[] = [];
    const accounts: Partial<Record<ConnectorCapabilityId, string>> = {};
    const connectorAccounts: Partial<
      Record<ConnectorCapabilityId, readonly ConnectorConnection[]>
    > = {};
    for (const id of CONNECTOR_CAPABILITY_IDS) {
      if (allowed && !allowed.has(id)) continue;
      const status = projectConnectorStatus(statuses[id], allowedConnections?.[id]);
      if (!status.connected) continue;
      authenticated.push(id);
      if (status.account !== undefined) accounts[id] = status.account;
      // Owner sessions may select any listed connection. New delegated grants
      // expose only their exact approved IDs. Legacy capability-only grants keep
      // their summary fields but do not gain a selectable connection surface.
      if (status.connections !== undefined
        && (allowedConnectors === undefined || allowedConnections !== undefined)) {
        connectorAccounts[id] = status.connections;
      }
    }
    return {
      status: "ready",
      authenticated,
      accounts,
      connectorAccounts,
      identity: {},
      stablecoins: [],
      authorizations: [],
    };
  } catch {
    return emptyInfo("unavailable");
  }
}

export function projectAccountInfo(
  info: AccountInfo,
  allowedConnectors?: readonly ConnectorCapabilityId[],
  allowedConnections?: ConnectorConnectionSelection,
): AccountInfo {
  if (allowedConnectors === undefined) {
    return { ...info, connectorAccounts: info.connectorAccounts ?? {} };
  }
  const allowed = new Set(allowedConnectors);
  const connectorAccounts = Object.fromEntries(
    Object.entries(info.connectorAccounts ?? {}).flatMap(([id, connections]) => {
      const capability = id as ConnectorCapabilityId;
      if (!allowed.has(capability) || allowedConnections === undefined) return [];
      const selected = new Set(allowedConnections[capability] ?? []);
      return [[capability, connections.filter(({ id: connectionId }) => selected.has(connectionId))]];
    }),
  );
  const authenticated = info.authenticated.filter((id) => {
    if (!allowed.has(id)) return false;
    const original = info.connectorAccounts?.[id];
    return original === undefined || allowedConnections === undefined
      ? true
      : (connectorAccounts[id] ?? []).length > 0;
  });
  const accounts = Object.fromEntries(Object.entries(info.accounts).filter(([id]) => (
    allowed.has(id as ConnectorCapabilityId)
  ))) as Partial<Record<ConnectorCapabilityId, string>>;
  if (allowedConnections !== undefined) {
    for (const id of CONNECTOR_CAPABILITY_IDS) {
      const connections = connectorAccounts[id];
      if (connections === undefined) continue;
      if (connections.length === 1) accounts[id] = connections[0]!.label;
      else delete accounts[id];
    }
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
    "again unless the task requires state refreshed after this first prompt. When connectorAccounts",
    "lists multiple connections for a service, choose the appropriate one by label and pass its id",
    "as X-Nanocodex-Connector-Connection on that provider request. Never invent a connection id.",
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
