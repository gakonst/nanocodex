/** Hosted MCP OAuth uses a Tempo Wallet approval; the public default is discovery-only. */
export const MERCATOR_OAUTH_MCP_URL = "https://mercator.sh/mcp/auth";

export function needsMercatorOnboarding(connections: readonly { name: string }[]): boolean {
  return !connections.some(({ name }) => name === "Mercator");
}
