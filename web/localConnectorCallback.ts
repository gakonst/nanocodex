import {
  isLocalNanocodexOrigin,
  localMcpOAuthRelayCallbackUrl,
  localOAuthRelayCallbackUrl,
  signLocalOAuthRelayState,
} from "./localOAuthRelayEnvelope.mjs";

export type LocalConnectorFlow = "connect" | "managed";

export type LocalConnectorAuthorization = Readonly<{
  connector: "github" | "gmail" | "gdrive" | "x" | "slack";
  redirectUri: string;
  targetOrigin: string;
  flow: LocalConnectorFlow;
}>;

export type LocalMcpAuthorization = Readonly<{
  connectionId: string;
  redirectUri: string;
  targetOrigin: string;
  flow: LocalConnectorFlow;
}>;

const CONNECTOR = /^(github|gmail|gdrive|x|slack)$/;
const CONNECTOR_RETURN = /^\/v1\/connect\/auth\/connector-callback\/(github|gmail|gdrive|x|slack)$/;
const MCP_CONNECTION = /^[A-Za-z0-9_-]{43}$/;
const MCP_CONNECTION_RETURN = /^\/v1\/connect\/auth\/mcp-connection-callback\/([A-Za-z0-9_-]{43})$/;
const MAX_STATE_LENGTH = 512;

export function localConnectorAuthorization(
  targetOrigin: string,
  connector: string,
  flow: LocalConnectorFlow,
): LocalConnectorAuthorization | undefined {
  if (!CONNECTOR.test(connector) || !isLocalNanocodexOrigin(targetOrigin)) return undefined;
  const redirectUri = localOAuthRelayCallbackUrl(connector);
  if (!redirectUri) return undefined;
  return {
    connector: connector as LocalConnectorAuthorization["connector"],
    redirectUri,
    targetOrigin: new URL(targetOrigin).origin,
    flow,
  };
}

export function localMcpAuthorization(
  targetOrigin: string,
  connectionId: string,
  flow: LocalConnectorFlow,
): LocalMcpAuthorization | undefined {
  if (!MCP_CONNECTION.test(connectionId) || !isLocalNanocodexOrigin(targetOrigin)) return undefined;
  const redirectUri = localMcpOAuthRelayCallbackUrl(connectionId);
  if (!redirectUri) return undefined;
  return {
    connectionId,
    redirectUri,
    targetOrigin: new URL(targetOrigin).origin,
    flow,
  };
}

export async function wrapLocalConnectorAuthorizationState(
  authorizationUrl: URL,
  local: LocalConnectorAuthorization,
  relayKey: string,
): Promise<URL> {
  const state = authorizationUrl.searchParams.get("state");
  if (!state || state.length > MAX_STATE_LENGTH) {
    throw new Error("invalid local connector authorization state");
  }
  authorizationUrl.searchParams.set("state", await signLocalOAuthRelayState({
    provider: local.connector,
    targetOrigin: local.targetOrigin,
    flow: local.flow,
    state,
  }, relayKey));
  return authorizationUrl;
}

export async function wrapLocalMcpAuthorizationState(
  authorizationUrl: URL,
  local: LocalMcpAuthorization,
  relayKey: string,
): Promise<URL> {
  const state = authorizationUrl.searchParams.get("state");
  if (!state || state.length > MAX_STATE_LENGTH) {
    throw new Error("invalid local MCP authorization state");
  }
  authorizationUrl.searchParams.set("state", await signLocalOAuthRelayState({
    connectionId: local.connectionId,
    targetOrigin: local.targetOrigin,
    flow: local.flow,
    state,
  }, relayKey));
  return authorizationUrl;
}

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
