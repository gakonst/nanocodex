const managedBaseCapabilities = ["agents:read", "agents:write", "tools:use"];
const managedOptionalCapabilities = ["history:read", "memory:read", "memory:write"];
const managedPortabilityGrantCapabilities = [
  "agent.durability.portability",
  "agent.history.read",
  "agent.trace.read",
];

export function managedAgentPortabilityGranted(capabilities) {
  const granted = new Set(capabilities);
  return managedPortabilityGrantCapabilities.every((capability) => granted.has(capability));
}

export function managedGrantHeaders(assertion) {
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
    ...(assertion.appToolCatalogDigest === undefined
      ? {}
      : { "x-nanocodex-connect-app-tool-catalog-digest": assertion.appToolCatalogDigest }),
  };
}

export function managedGrantWebSocketHeaders(assertion, origin) {
  return {
    ...managedGrantHeaders(assertion),
    origin,
    upgrade: "websocket",
  };
}

export function managedGrantUpstreamMethod(method, resource) {
  if (method !== "POST") return method;
  return resource === ""
    || resource === "/events"
    || resource === "/events/history"
    || /^\/turns\/[^/]+$/.test(resource)
    ? "GET"
    : method;
}

export function managedAgentExistenceStatus(response) {
  if (response.status === 204) return "available";
  if (response.status === 404) return "missing";
  return "unavailable";
}
