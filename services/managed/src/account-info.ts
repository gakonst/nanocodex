import type { PromptInput } from "nanocodex";

const CONNECTOR_IDS = ["github", "gmail", "gdrive", "x"] as const;

type ConnectorId = typeof CONNECTOR_IDS[number];
type ConnectorReference = ConnectorId | `slack:${string}`;
type BrokerBinding = Readonly<{
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
}>;

export type AccountInfo = Readonly<{
  status: "disabled" | "ready" | "unavailable";
  authenticated: readonly ConnectorReference[];
  accounts: Readonly<Partial<Record<ConnectorReference, string>>>;
  identity: Readonly<Record<string, never>>;
  stablecoins: readonly [];
  authorizations: readonly [];
}>;

export async function accountInfo(
  binding: BrokerBinding,
  userId: string,
  enabled: boolean,
  allowedConnectors?: readonly ConnectorReference[],
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
    const accounts: Partial<Record<ConnectorReference, string>> = {};
    const allowed = allowedConnectors === undefined ? undefined : new Set(allowedConnectors);
    const authenticated: ConnectorReference[] = CONNECTOR_IDS.filter((id) => {
      if (allowed && !allowed.has(id)) return false;
      const connector = connectors[id];
      if (!isRecord(connector) || connector.connected !== true) return false;
      if (typeof connector.label === "string" && connector.label.trim()) {
        accounts[id] = connector.label.trim();
      }
      return true;
    });
    const slack = connectors.slack;
    if (isRecord(slack) && Array.isArray(slack.connections)) {
      for (const connection of slack.connections.slice(0, 64)) {
        if (!isRecord(connection) || typeof connection.id !== "string"
          || !/^[A-Z0-9]{1,32}$/.test(connection.id)) continue;
        const reference = `slack:${connection.id}` as const;
        if (allowed && !allowed.has(reference)) continue;
        authenticated.push(reference);
        if (typeof connection.label === "string" && connection.label.trim()) {
          accounts[reference] = connection.label.trim();
        }
      }
    }
    return projectAccountInfo({
      status: "ready",
      authenticated,
      accounts,
      identity: {},
      stablecoins: [],
      authorizations: [],
    }, allowedConnectors);
  } catch {
    return emptyInfo("unavailable");
  }
}

export function projectAccountInfo(
  info: AccountInfo,
  allowedConnectors?: readonly ConnectorReference[],
): AccountInfo {
  if (allowedConnectors === undefined) return info;
  const allowed = new Set(allowedConnectors);
  return {
    ...info,
    authenticated: info.authenticated.filter((id) => allowed.has(id)),
    accounts: Object.fromEntries(Object.entries(info.accounts).filter(([id]) => (
      allowed.has(id as ConnectorReference)
    ))),
  };
}

export function withInitialAccountInfo(input: PromptInput, info: AccountInfo): PromptInput {
  const explanation = [
    "The managed runtime already resolved the following non-secret accountInfo snapshot for",
    "this agent. Use it as the current connected-account context. Do not call accountInfo",
    "again unless the task requires state refreshed after this first prompt.",
    "Slack accounts are named slack:<workspace-id>; send that workspace ID in",
    "x-nanocodex-connector-instance on requests to https://slack.com/api/<method>.",
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
    identity: {},
    stablecoins: [],
    authorizations: [],
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
