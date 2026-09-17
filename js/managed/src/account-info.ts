import { isVmFactoryName } from "./vm-factory-name";
import { connectorToolMetadata } from "./connector-tools";
import { performanceStage } from "./performance";
import type { X_API } from "nanocodex-tools/x";
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
      browser_origin?: string;
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
  /** Logical namespace root. Native host workspace paths are never projected. */
  mount: string;
  /** Retained identity paths accepted by native execution for older prompts. */
  aliases?: readonly string[];
  /** Current attachment presence for user hands; absent when not known. */
  online?: boolean;
  /** Exact mount provider advertised by this computer; allocation checks live capacity. */
  vm_provider?: string;
} & (
  | { kind: "sandbox"; provider: string }
  | { kind: "user"; provider?: never }
)>;

export type AccountInfo = Readonly<{
  status: "disabled" | "ready" | "unavailable";
  /** Native public APIs available independently of account connectors. */
  apis: readonly (typeof X_API)[];
  /** Legacy capability-level summary retained for existing agents. */
  authenticated: readonly ConnectorCapabilityId[];
  /** Legacy single-account labels retained when a capability has one visible account. */
  accounts: Readonly<Partial<Record<ConnectorCapabilityId, string>>>;
  /** Provider-neutral, selectable connection metadata keyed by service capability. */
  connectorAccounts: Readonly<
    Partial<Record<ConnectorCapabilityId, readonly ConnectorConnection[]>>
  >;
  /** Discoverable first-party tools for currently connected, permitted services. */
  connectorTools: ReturnType<typeof connectorToolMetadata>;
  /** Known hands, including retained user hands whose attachment is offline. */
  machines: readonly AccountMachine[];
  identity: Readonly<Record<string, never>>;
  stablecoins: readonly [];
  authorizations: readonly [];
  vault: readonly VaultEntry[];
}>;

export type AccountInfoOptions = Readonly<{
  /** A live or bounded owner/authority-scoped discovery snapshot. */
  catalog?: Promise<unknown>;
  allowedConnectors?: readonly ConnectorCapabilityId[];
  allowedConnections?: ConnectorConnectionSelection;
  enabled: boolean;
  apis?: readonly (typeof X_API)[];
  machines?: readonly AccountMachine[];
  signal?: AbortSignal;
}>;

export async function accountInfo(
  binding: BrokerBinding,
  userId: string,
  {
    allowedConnectors,
    allowedConnections,
    enabled,
    apis = [],
    machines = [],
    signal,
    catalog,
  }: AccountInfoOptions,
): Promise<AccountInfo> {
  machines = projectHandProviders(machines);
  if (!enabled) return emptyInfo("disabled", machines, apis);
  signal?.throwIfAborted();
  try {
    const encodedUserId = encodeURIComponent(userId);
    const [connectorMetadata, vault] = await Promise.all([
      catalog ?? (async () => {
        const response = await (signal === undefined
          ? binding.fetch(`https://broker.internal/users/${encodedUserId}/connectors`)
          : binding.fetch(`https://broker.internal/users/${encodedUserId}/connectors`, { signal }));
        if (!response.ok) {
          await response.body?.cancel();
          throw new Error("account connector status unavailable");
        }
        return response.json();
      })(),
      performanceStage("account.vault", () => accountVault(binding, encodedUserId, signal)),
    ]);
    signal?.throwIfAborted();
    const statuses = connectorStatuses(connectorMetadata);
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
      apis,
      authenticated,
      accounts,
      connectorAccounts,
      connectorTools: connectorToolMetadata(authenticated),
      machines,
      identity: {},
      stablecoins: [],
      authorizations: [],
      vault,
    };
  } catch {
    signal?.throwIfAborted();
    return emptyInfo("unavailable", machines, apis);
  }
}

/** A display name never grants placement authority. Select the exact provider
 * from the current machine catalog, and never promote a retained offline row. */
export function projectHandProviders(machines: readonly AccountMachine[]): readonly AccountMachine[] {
  return machines.map(machine => {
    const { vm_provider: _, ...base } = machine;
    const providers = machine.capabilities.filter(value => value.startsWith("vm_factory:"))
      .map(value => value.slice("vm_factory:".length)).filter(isVmFactoryName);
    return machine.online === true && providers.length === 1
      ? { ...base, vm_provider: providers[0] } : base;
  });
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
      apis: info.apis ?? [],
      connectorAccounts: info.connectorAccounts ?? {},
      connectorTools: connectorToolMetadata(info.authenticated),
      machines: projectHandProviders(info.machines ?? []),
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
    apis: info.apis ?? [],
    authenticated,
    accounts,
    connectorAccounts,
    connectorTools: connectorToolMetadata(authenticated),
    vault,
    machines: projectHandProviders(info.machines ?? []),
  };
}

function emptyInfo(
  status: "disabled" | "unavailable",
  machines: readonly AccountMachine[],
  apis: readonly (typeof X_API)[],
): AccountInfo {
  return {
    status,
    apis,
    authenticated: [],
    accounts: {},
    connectorAccounts: {},
    connectorTools: {},
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
  signal?: AbortSignal,
): Promise<readonly VaultEntry[]> {
  signal?.throwIfAborted();
  try {
    const url = `https://broker.internal/users/${encodedUserId}/credentials/vault`;
    const response = signal === undefined
      ? await binding.fetch(url)
      : await binding.fetch(url, { signal });
    if (!response.ok) {
      await response.body?.cancel();
      return [];
    }
    const value: unknown = await response.json();
    return isRecord(value) ? vaultEntries(value.vault) : [];
  } catch {
    signal?.throwIfAborted();
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
    && exactKeys(value, ["id", "kind", "name", "created_at", "username", ...(value.browser_origin === undefined ? [] : ["browser_origin"])])
    && vaultText(value.username, 512)) {
    if (value.browser_origin !== undefined && !safeBrowserOrigin(value.browser_origin)) return undefined;
    return { ...common, kind: "login", username: value.username,
      ...(typeof value.browser_origin === "string" ? { browser_origin: value.browser_origin } : {}) };
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

function safeBrowserOrigin(value: unknown): value is string {
  if (typeof value !== "string" || value.length > 2048) return false;
  try { const url = new URL(value); return url.protocol === "https:" && url.origin === value; } catch { return false; }
}
