export function presentGrantCapabilities(capabilities: readonly string[]): string[] {
  const visible = [];
  let remoteMcpCount = 0;
  for (const capability of capabilities) {
    if (capability.startsWith("mcp:")) {
      remoteMcpCount += 1;
      continue;
    }
    visible.push(capability);
  }
  if (remoteMcpCount > 0) {
    visible.push(remoteMcpCount === 1
      ? "Remote MCP connection"
      : `Remote MCP connections (${remoteMcpCount})`);
  }
  return visible;
}
