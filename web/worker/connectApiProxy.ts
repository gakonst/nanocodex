import { isScopedConnectConnectorState } from "../connectConnectorCallback.mjs";

export type ConnectApiProxyEnv = {
  ENVIRONMENT?: string;
  NANOCODEX_CONNECT_API?: Fetcher;
};

const CLI_APP_ID = "nanocodex-cli";
const CONNECT_BROWSER_CLIENTS = new Set(["device", "onboarding"]);
const CONNECT_BROWSER_ROUTE = /^(?:\/v1\/device(?:\/.*)?|\/v1\/connect\/auth(?:\/.*)?|\/v1\/hosted-authorizations|\/v1\/account-link|\/v1\/connections(?:\/.*)?|\/v1\/access-keys(?:\/.*)?|\/v1\/grants(?:\/.*)?)$/;
const CONNECTOR_ROUTE = /^\/v1\/connectors(?:\/.*)?$/;
const CONNECTOR_CALLBACK = /^\/v1\/connectors\/(?:github|gmail|gdrive|x|whoop)\/callback$/;
const MCP_CONNECTION_ROUTE = /^\/v1\/mcp-connections(?:\/.*)?$/;
const MCP_CONNECTION_CALLBACK = /^\/v1\/mcp-connections\/[A-Za-z0-9_-]{43}\/callback$/;

/**
 * Projects the Connect Worker onto the canonical Nanocodex browser origin.
 *
 * Overlapping managed routes are selected only by an explicit onboarding
 * marker, a Connect-scoped OAuth state, or the CLI's signed app identity.
 */
export async function routeConnectApi(
  request: Request,
  env: ConnectApiProxyEnv,
  url: URL,
): Promise<Response | undefined> {
  if (!env.NANOCODEX_CONNECT_API
    || !isConnectApiRequest(request, url.pathname)) {
    return undefined;
  }
  const upstreamUrl = new URL(url);
  if (url.pathname === "/api/connect/health") upstreamUrl.pathname = "/healthz";
  try {
    return await env.NANOCODEX_CONNECT_API.fetch(new Request(upstreamUrl, request));
  } catch (error) {
    console.error(JSON.stringify({
      type: "connect_api.backend_failure",
      path: url.pathname,
      error: error instanceof Error
        ? { name: error.name }
        : { name: typeof error },
    }));
    return Response.json({ error: "connect_api_unavailable" }, {
      status: 503,
      headers: {
        "cache-control": "no-store",
        "x-content-type-options": "nosniff",
      },
    });
  }
}

export function isConnectApiRequest(
  request: Request,
  pathname: string,
): boolean {
  if (isConnectApiBrowserRoutePath(pathname)) return true;
  if (CONNECTOR_ROUTE.test(pathname)
    && CONNECT_BROWSER_CLIENTS.has(request.headers.get("x-nanocodex-connect-client") ?? "")) return true;
  if (CONNECTOR_CALLBACK.test(pathname)
    && isScopedConnectConnectorState(new URL(request.url).searchParams.get("state"))) return true;
  if (MCP_CONNECTION_CALLBACK.test(pathname)) return true;
  if (MCP_CONNECTION_ROUTE.test(pathname)
    && CONNECT_BROWSER_CLIENTS.has(request.headers.get("x-nanocodex-connect-client") ?? "")) return true;
  return request.headers.get("x-nanocodex-app-id") === CLI_APP_ID;
}

export function isConnectApiBrowserRoutePath(
  pathname: string,
): boolean {
  return pathname === "/api/connect/health"
    || CONNECT_BROWSER_ROUTE.test(pathname);
}

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;
