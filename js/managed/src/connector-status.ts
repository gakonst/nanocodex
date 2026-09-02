export const CONNECTOR_CAPABILITY_IDS = [
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
] as const;

export const CONNECTOR_PROVIDER_IDS = ["github", "google", "slack", "x"] as const;

export type ConnectorCapabilityId = typeof CONNECTOR_CAPABILITY_IDS[number];
export type ConnectorProviderId = typeof CONNECTOR_PROVIDER_IDS[number];
export type ConnectorConnection = Readonly<{
  id: string;
  label: string;
  accountId?: string;
  capabilities?: readonly ConnectorCapabilityId[];
}>;
export type ConnectorStatus = Readonly<{
  connected: boolean;
  connections?: readonly ConnectorConnection[];
  /** Compatibility projection for legacy singleton status readers. */
  account?: string;
}>;
export type ConnectorConnectionSelection = Readonly<
  Partial<Record<ConnectorCapabilityId, readonly string[]>>
>;

const CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const MAX_CONNECTIONS = 64;

export function connectorCapabilityId(value: unknown): ConnectorCapabilityId | undefined {
  return CONNECTOR_CAPABILITY_IDS.find((id) => id === value);
}

export function connectorProviderId(value: unknown): ConnectorProviderId | undefined {
  if (value === "gmail" || value === "gdrive") return "google";
  return CONNECTOR_PROVIDER_IDS.find((id) => id === value);
}

export function connectorConnectionId(value: unknown): string | undefined {
  return typeof value === "string" && CONNECTION_ID.test(value) ? value : undefined;
}

/** Sanitizes one broker status while retaining legacy singleton projections. */
export function connectorStatus(value: unknown): ConnectorStatus {
  if (!isRecord(value) || value.connected !== true) {
    return { connected: false, connections: [] };
  }
  if (Array.isArray(value.connections)) {
    if (value.connections.length > MAX_CONNECTIONS) {
      throw new Error("connector status returned too many connections");
    }
    const connections = value.connections.map(publicConnectorConnection);
    if (new Set(connections.map(({ id }) => id)).size !== connections.length) {
      throw new Error("connector status returned duplicate connections");
    }
    return {
      connected: connections.length > 0,
      connections,
      ...(connections.length === 1 ? { account: connections[0]!.label } : {}),
    };
  }

  // Older brokers exposed only one top-level label/account_id and no selectable ID.
  // Keep that account visible, but do not manufacture an ID that could be sent back
  // to the credential boundary.
  const account = boundedString(value.label, 256) ?? boundedString(value.account_id, 256);
  return {
    connected: true,
    ...(account === undefined ? {} : { account }),
  };
}

export function connectorStatuses(
  value: unknown,
): Record<ConnectorCapabilityId, ConnectorStatus> {
  if (!isRecord(value) || !isRecord(value.connectors)) {
    throw new Error("connector listing returned an invalid response");
  }
  const connectors = value.connectors;
  return Object.fromEntries(CONNECTOR_CAPABILITY_IDS.map((id) => [
    id,
    connectorStatus(connectors[id]),
  ])) as Record<ConnectorCapabilityId, ConnectorStatus>;
}

export function projectConnectorStatus(
  status: ConnectorStatus,
  allowedIds: readonly string[] | undefined,
): ConnectorStatus {
  if (allowedIds === undefined) return status;
  if (status.connections === undefined) {
    // A legacy capability-level grant can observe its singleton label, but it
    // cannot select an unbound account ID.
    return status;
  }
  const allowed = new Set(allowedIds);
  const connections = status.connections.filter(({ id }) => allowed.has(id));
  return {
    connected: connections.length > 0,
    connections,
    ...(connections.length === 1 ? { account: connections[0]!.label } : {}),
  };
}

function publicConnectorConnection(value: unknown): ConnectorConnection {
  if (!isRecord(value)) throw new Error("connector status returned an invalid connection");
  const id = connectorConnectionId(value.id);
  const label = boundedString(value.label, 256);
  const accountId = value.account_id === undefined
    ? undefined
    : boundedString(value.account_id, 256);
  const capabilities = value.capabilities === undefined
    ? undefined
    : connectorCapabilities(value.capabilities);
  if (!id || !label || (value.account_id !== undefined && !accountId)) {
    throw new Error("connector status returned an invalid connection");
  }
  return {
    id,
    label,
    ...(accountId === undefined ? {} : { accountId }),
    ...(capabilities === undefined ? {} : { capabilities }),
  };
}

function connectorCapabilities(value: unknown): readonly ConnectorCapabilityId[] {
  if (!Array.isArray(value) || value.length > CONNECTOR_CAPABILITY_IDS.length) {
    throw new Error("connector status returned invalid capabilities");
  }
  const capabilities = value.map(connectorCapabilityId);
  if (capabilities.some((id) => id === undefined)
    || new Set(capabilities).size !== capabilities.length) {
    throw new Error("connector status returned invalid capabilities");
  }
  return capabilities as ConnectorCapabilityId[];
}

function boundedString(value: unknown, maxLength: number): string | undefined {
  if (typeof value !== "string") return undefined;
  const normalized = value.trim();
  return normalized.length > 0 && normalized.length <= maxLength ? normalized : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
