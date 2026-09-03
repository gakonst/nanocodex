import type { PromptInput } from "nanocodex";
import type { HostedMachine } from "./hosted-tools-protocol";

import {
  CONNECTOR_CAPABILITY_IDS,
  connectorStatuses,
  projectConnectorStatus,
  type ConnectorCapabilityId,
  type ConnectorConnection,
  type ConnectorConnectionSelection,
} from "./connector-status";

const MAX_VAULT_ENTRIES = 100;
const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;

type BrokerBinding = Readonly<{
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
}>;

export type VaultEntry =
  | Readonly<{
      id: string;
      kind: "login";
      name: string;
      created_at: number;
      username: string;
    }>
  | Readonly<{
      id: string;
      kind: "card";
      name: string;
      created_at: number;
      last4: string;
    }>
  | Readonly<{
      id: string;
      kind: "address";
      name: string;
      created_at: number;
      address_line_1: string;
      address_line_2?: string;
      city: string;
      state: string;
      zip: string;
      country: string;
    }>
  | Readonly<{
      id: string;
      kind: "phone";
      name: string;
      created_at: number;
      phone_number: string;
    }>;

export type AccountMachine = Readonly<HostedMachine & {
  kind: "sandbox" | "user";
  /** Logical namespace root. Native host workspace paths are never projected. */
  mount: string;
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
  /** Hands currently available to the account-owned agent. */
  machines: readonly AccountMachine[];
  identity: Readonly<Record<string, never>>;
  stablecoins: readonly [];
  authorizations: readonly [];
  vault: readonly VaultEntry[];
}>;

export async function accountInfo(
  binding: BrokerBinding,
  userId: string,
  enabled: boolean,
  allowedConnectors?: readonly ConnectorCapabilityId[],
  allowedConnections?: ConnectorConnectionSelection,
  machines: readonly AccountMachine[] = [],
): Promise<AccountInfo> {
  if (!enabled) return emptyInfo("disabled", machines);
  try {
    const encodedUserId = encodeURIComponent(userId);
    const [response, vault] = await Promise.all([
      binding.fetch(`https://broker.internal/users/${encodedUserId}/connectors`),
      accountVault(binding, encodedUserId),
    ]);
    if (!response.ok) {
      await response.body?.cancel();
      return emptyInfo("unavailable", machines);
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
      machines,
      identity: {},
      stablecoins: [],
      authorizations: [],
      vault,
    };
  } catch {
    return emptyInfo("unavailable", machines);
  }
}

export function projectAccountInfo(
  info: AccountInfo,
  allowedConnectors?: readonly ConnectorCapabilityId[],
  allowedConnections?: ConnectorConnectionSelection,
): AccountInfo {
  const vault = vaultEntries(info.vault);
  if (allowedConnectors === undefined) {
    return {
      ...info,
      connectorAccounts: info.connectorAccounts ?? {},
      machines: info.machines ?? [],
      vault,
    };
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
    vault,
    machines: info.machines ?? [],
  };
}

export function withInitialAccountInfo(input: PromptInput, info: AccountInfo): PromptInput {
  const explanation = [
    "The managed runtime already resolved the following non-secret accountInfo snapshot for",
    "this agent. Use it as the current connected-account context. Do not call accountInfo",
    "again unless the task requires state refreshed after this first prompt. Machine topology is",
    "intentionally omitted from this retained snapshot: call accountInfo immediately before choosing",
    "a hand because user machines can connect or disconnect without restarting the agent. When connectorAccounts",
    "lists multiple connections for a service, choose the appropriate one by label and pass its id",
    "as X-Nanocodex-Connector-Connection on that provider request. Never invent a connection id.",
    "Vault entries are safe references only and never contain passwords, full card numbers, CVVs,",
    "expiry details,",
    "or billing ZIPs.",
  ].join(" ");
  const context = {
    type: "text" as const,
    text: `${explanation}\n\n<account_info>\n${JSON.stringify({ ...info, machines: [] })}\n</account_info>`,
  };
  return typeof input === "string"
    ? [context, { type: "text", text: input }]
    : [context, ...input];
}

function emptyInfo(
  status: "disabled" | "unavailable",
  machines: readonly AccountMachine[],
): AccountInfo {
  return {
    status,
    authenticated: [],
    accounts: {},
    connectorAccounts: {},
    machines,
    identity: {},
    stablecoins: [],
    authorizations: [],
    vault: [],
  };
}

async function accountVault(
  binding: BrokerBinding,
  encodedUserId: string,
): Promise<readonly VaultEntry[]> {
  try {
    const response = await binding.fetch(
      `https://broker.internal/users/${encodedUserId}/credentials`,
    );
    if (!response.ok) {
      await response.body?.cancel();
      return [];
    }
    const value: unknown = await response.json();
    return isRecord(value) ? vaultEntries(value.vault) : [];
  } catch {
    return [];
  }
}

function vaultEntries(value: unknown): readonly VaultEntry[] {
  if (!Array.isArray(value) || value.length > MAX_VAULT_ENTRIES) return [];
  const projected: VaultEntry[] = [];
  for (const entry of value) {
    const safe = vaultEntry(entry);
    if (!safe) return [];
    projected.push(safe);
  }
  return projected;
}

function vaultEntry(value: unknown): VaultEntry | undefined {
  if (!isRecord(value)
    || typeof value.id !== "string" || !VAULT_ID.test(value.id)
    || !vaultText(value.name, 120)
    || !Number.isSafeInteger(value.created_at)
    || Number(value.created_at) < 0) return undefined;
  const common = {
    id: value.id,
    name: value.name,
    created_at: value.created_at as number,
  };
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
      ...(hasLine2 ? { address_line_2: value.address_line_2 as string } : {}),
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

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const keys = Object.keys(value);
  return keys.length === expected.length && keys.every((key) => expected.includes(key));
}

function vaultText(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.length > 0 && value.trim() === value
    && !/[\u0000-\u001f\u007f]/.test(value)
    && new TextEncoder().encode(value).byteLength <= maxBytes;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
