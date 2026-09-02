import type { HostPrincipal } from "./hostPrincipal.mts";

const managedBaseCapabilities = ["agents:read", "agents:write", "tools:use"];
const managedOptionalCapabilities = ["history:read", "memory:read", "memory:write"];
const managedPortabilityGrantCapabilities = [
  "agent.durability.portability",
  "agent.history.read",
  "agent.trace.read",
];

export type ManagedGrantAssertion = Readonly<{
  appToolCatalogDigest?: `0x${string}`;
  brokerUserId: string;
  capabilities: readonly string[];
  connectors: readonly string[];
  connectorConnections?: Readonly<Record<string, readonly string[]>>;
  grantId: `0x${string}`;
  mcpIds: readonly string[];
  hostPrincipal?: HostPrincipal;
}>;

export function managedAgentPortabilityGranted(capabilities: readonly string[]): boolean {
  const granted = new Set(capabilities);
  return managedPortabilityGrantCapabilities.every((capability) => granted.has(capability));
}

export function managedGrantHeaders(assertion: ManagedGrantAssertion): Record<string, string> {
  const granted = new Set(assertion.capabilities);
  const portability = managedAgentPortabilityGranted(assertion.capabilities);
  return {
    "x-nanocodex-connect-user": assertion.brokerUserId,
    "x-nanocodex-connect-grant-id": assertion.grantId,
    "x-nanocodex-connect-capabilities": JSON.stringify([
      ...managedBaseCapabilities,
      ...managedOptionalCapabilities.filter((capability) => granted.has(capability)),
      ...(portability ? ["agents:portability"] : []),
    ]),
    "x-nanocodex-connect-connectors": JSON.stringify(assertion.connectors),
    ...(assertion.connectorConnections === undefined ? {} : {
      "x-nanocodex-connect-connector-connections": JSON.stringify(assertion.connectorConnections),
    }),
    "x-nanocodex-connect-mcp-ids": JSON.stringify(assertion.mcpIds),
    ...(assertion.hostPrincipal === undefined
      ? {}
      : { "x-nanocodex-connect-host-principal": JSON.stringify(assertion.hostPrincipal) }),
    ...(assertion.appToolCatalogDigest === undefined
      ? {}
      : { "x-nanocodex-connect-app-tool-catalog-digest": assertion.appToolCatalogDigest }),
  };
}

export function managedGrantWebSocketHeaders(
  assertion: ManagedGrantAssertion,
  origin: string,
): Record<string, string> {
  return {
    ...managedGrantHeaders(assertion),
    origin,
    upgrade: "websocket",
  };
}

export function managedGrantUpstreamMethod(method: string, resource: string): string {
  if (method !== "POST") return method;
  return resource === ""
    || resource === "/events"
    || resource === "/events/history"
    || /^\/turns\/[^/]+$/.test(resource)
    ? "GET"
    : method;
}

export function managedAgentExistenceStatus(
  response: Response,
): "available" | "missing" | "unavailable" {
  if (response.status === 204) return "available";
  if (response.status === 404) return "missing";
  return "unavailable";
}
