export type ConnectorCapability =
  | "github"
  | "gmail"
  | "gdrive"
  | "gcalendar"
  | "gtasks"
  | "gdocs"
  | "gsheets"
  | "gslides"
  | "gcontacts"
  | "slack"
  | "x"
  | "chatgpt";

export type ConnectorProvider = "github" | "google" | "slack" | "x" | "chatgpt";

export type ConnectorConnection = Readonly<{
  id: string;
  label: string;
  account_id?: string | undefined;
  capabilities: readonly ConnectorCapability[];
}>;

export type ConnectorStatus = Readonly<{
  connected: boolean;
  connections: readonly ConnectorConnection[];
  account_id?: string | undefined;
  connection_id?: string | undefined;
  label?: string | undefined;
}>;

export type ConnectorStatuses = Readonly<Partial<Record<ConnectorCapability, ConnectorStatus>>>;
export type ConnectorControl = Readonly<{
  provider: ConnectorProvider;
  capabilities: readonly ConnectorCapability[];
  connectedCapabilities: readonly ConnectorCapability[];
  missingCapabilities: readonly ConnectorCapability[];
  connections: readonly ConnectorConnection[];
  connected: boolean;
  partial: boolean;
  resolved: boolean;
}>;

export const googleConnectorCapabilities: readonly [
  "gmail", "gdrive", "gcalendar", "gtasks", "gdocs", "gsheets", "gslides", "gcontacts",
];
export const connectorCapabilityIds: readonly ConnectorCapability[];
export const connectorProviderIds: readonly ConnectorProvider[];
export const connectorConnectionHeader: "X-Nanocodex-Connector-Connection";
export function connectorProviderFor(capability: unknown): ConnectorProvider | undefined;
export function connectorCapabilityLabel(capability: unknown): string | undefined;
export function connectorStatusesFromWire(value: unknown): ConnectorStatuses;
export function connectorConnectionsForCapabilities(
  statuses: unknown,
  capabilities: readonly ConnectorCapability[],
): readonly ConnectorConnection[];
export function connectorProviderMatchesCapabilities(
  provider: unknown,
  capabilities: readonly string[],
): boolean;
export function connectorControlsForCapabilities(
  capabilities: readonly ConnectorCapability[],
  statuses?: unknown,
): readonly ConnectorControl[];
