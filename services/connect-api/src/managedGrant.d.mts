export type ManagedGrantAssertion = Readonly<{
  appToolCatalogDigest?: `0x${string}`;
  brokerUserId: string;
  capabilities: readonly string[];
  connectors: readonly string[];
  connectorConnections?: Readonly<Record<string, readonly string[]>>;
  grantId: `0x${string}`;
  mcpIds: readonly string[];
}>;

export function managedAgentPortabilityGranted(capabilities: readonly string[]): boolean;
export function managedGrantHeaders(
  assertion: ManagedGrantAssertion,
): Record<string, string>;
export function managedGrantWebSocketHeaders(
  assertion: ManagedGrantAssertion,
  origin: string,
): Record<string, string>;
export function managedGrantUpstreamMethod(method: string, resource: string): string;
