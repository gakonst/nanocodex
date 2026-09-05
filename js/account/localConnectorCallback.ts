import type { LocalConnectorFlow } from "nanocodex-vite/oauth-relay";

const CONNECTOR_RETURN = /^\/v1\/connect\/auth\/connector-callback\/(github|google|gmail|gdrive|slack|x)$/;
const MCP_CONNECTION_RETURN = /^\/v1\/connect\/auth\/mcp-connection-callback\/([A-Za-z0-9_-]{43})$/;

export function localConnectorCallbackReturn(url: URL): Readonly<{
  callbackUrl: URL;
  flow: LocalConnectorFlow;
}> | undefined {
  const match = url.pathname.match(CONNECTOR_RETURN);
  const mcpMatch = url.pathname.match(MCP_CONNECTION_RETURN);
  if (!match && !mcpMatch) return undefined;
  const callbackUrl = new URL(match
    ? `/v1/connectors/${match[1]}/callback`
    : `/v1/mcp-connections/${mcpMatch![1]}/callback`, url.origin);
  callbackUrl.search = url.search;
  return { callbackUrl, flow: "connect" };
}
