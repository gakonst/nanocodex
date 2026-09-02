export const connectorCapabilities: readonly [
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
  "chatgpt",
];
export const oauthConnectorProviders: readonly ["github", "google", "slack", "x"];

export type ConnectorCapability = typeof connectorCapabilities[number];
export type OAuthConnectorProvider = typeof oauthConnectorProviders[number];
export type RoutableConnectorCapability = Exclude<ConnectorCapability, "chatgpt">;
export type ConnectorConnection = Readonly<{
  id: string;
  label: string;
  account_id?: string;
  capabilities?: readonly ConnectorCapability[];
}>;
export type ConnectorStatus = Readonly<{
  connected: boolean;
  connections: readonly ConnectorConnection[];
  label?: string;
  account_id?: string;
}>;
export type ConnectorConnectionSnapshot = Readonly<
  Partial<Record<ConnectorCapability, readonly string[]>>
>;

export class ConnectorPolicyFailure extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string, message: string);
}

export function isConnectorCapability(value: unknown): value is ConnectorCapability;
export function isConnectorConnectionId(value: unknown): value is string;
export function connectorProvider(value: unknown): OAuthConnectorProvider | undefined;
export function connectorCapabilityForUrl(url: URL): RoutableConnectorCapability | undefined;
export function connectorRequestTarget(
  capability: RoutableConnectorCapability,
  value: unknown,
): URL;
export function publicConnectorStatus(value: unknown): ConnectorStatus;
export function connectorConnectionSnapshot(
  statuses: Readonly<Record<ConnectorCapability, ConnectorStatus>>,
  capabilities?: readonly ConnectorCapability[],
): ConnectorConnectionSnapshot;
export function isConnectorConnectionSnapshot(value: unknown): value is ConnectorConnectionSnapshot;
export function intersectConnectorConnectionSnapshot(
  approved: ConnectorConnectionSnapshot | undefined,
  statuses: Readonly<Record<ConnectorCapability, ConnectorStatus>>,
  requested: readonly ConnectorCapability[],
): ConnectorConnectionSnapshot | undefined;
export function completeConnectorConnectionSnapshot(
  approved: ConnectorConnectionSnapshot | undefined,
  connectedAtApproval: readonly ConnectorCapability[] | undefined,
  statuses: Readonly<Record<ConnectorCapability, ConnectorStatus>>,
  requested: readonly ConnectorCapability[],
): Readonly<{
  connectorConnections: ConnectorConnectionSnapshot | undefined;
  legacyConnectorCapabilities: readonly ConnectorCapability[];
}>;
export function resolveConnectorConnection(
  snapshot: ConnectorConnectionSnapshot | undefined,
  capability: ConnectorCapability,
  bodySelector: unknown,
  headerSelector: unknown,
): string | undefined;
export function applyConnectorConnectionSelector(
  headers: Headers,
  snapshot: ConnectorConnectionSnapshot | undefined,
  capability: ConnectorCapability,
  bodySelector: unknown,
): string | undefined;
