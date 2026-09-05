const OPAQUE_ID = /^[A-Za-z0-9_-]{43}$/;
export { canonicalRemoteMcpTarget } from "../../mcp-target.mts";

export const mcpResourcePrefix = "urn:nanocodex:mcp:";
export const mcpFocusResourcePrefix = "urn:nanocodex:mcp-focus:";

export function isMcpConnectionId(value: unknown): value is string {
  return typeof value === "string" && OPAQUE_ID.test(value);
}

export function mcpConnectionIds(resources: unknown): readonly string[] {
  if (!Array.isArray(resources)) return [];
  return resources.flatMap((resource) => (
    typeof resource === "string" && resource.startsWith(mcpResourcePrefix)
      ? [resource.slice(mcpResourcePrefix.length)]
      : []
  )).filter(isMcpConnectionId);
}

export function focusedMcpConnectionIds(resources: unknown): readonly string[] {
  if (!Array.isArray(resources)) return [];
  return resources.flatMap((resource) => (
    typeof resource === "string" && resource.startsWith(mcpFocusResourcePrefix)
      ? [resource.slice(mcpFocusResourcePrefix.length)]
      : []
  )).filter(isMcpConnectionId);
}

export function isAllowedMcpResource(resource: unknown): boolean {
  if (typeof resource !== "string") return false;
  if (resource.startsWith(mcpResourcePrefix)) {
    return isMcpConnectionId(resource.slice(mcpResourcePrefix.length));
  }
  if (resource.startsWith(mcpFocusResourcePrefix)) {
    return isMcpConnectionId(resource.slice(mcpFocusResourcePrefix.length));
  }
  return false;
}

export function validateMcpResources(resources: unknown): Readonly<{
  requested: readonly string[];
  focus?: string;
}> {
  const requested = mcpConnectionIds(resources);
  const focused = focusedMcpConnectionIds(resources);
  if (new Set(requested).size !== requested.length
    || focused.length > 1
    || (focused[0] !== undefined && !requested.includes(focused[0]))) {
    throw new Error("The CLI MCP connection resources are invalid.");
  }
  return Object.freeze({
    requested: Object.freeze([...requested]),
    focus: focused[0],
  });
}
