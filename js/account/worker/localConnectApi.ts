export type LocalConnectApiEnv = {
  NANOCODEX_CONNECT_API?: LocalConnectApiFetcher;
};

export function routeLocalConnectApi(
  request: Request,
  env: LocalConnectApiEnv,
  url: URL,
): Promise<Response> | undefined {
  if (!isLocalConnectApiPath(url.pathname)) return undefined;
  // Connect and the account-owned browser runtime intentionally share the
  // public /v1/egress path. A Connect client carries its grant bearer token;
  // the browser shell carries only the persistent account cookie and must
  // fall through to the managed account router.
  if (url.pathname === "/v1/egress" && !request.headers.has("authorization")) {
    return undefined;
  }
  if (!env.NANOCODEX_CONNECT_API) return undefined;
  const headers = new Headers(request.headers);
  headers.set("x-nanocodex-local-origin", url.origin);
  return env.NANOCODEX_CONNECT_API.fetch(new Request(request, { headers }));
}

function isLocalConnectApiPath(pathname: string): boolean {
  return pathname === "/v1/account-link"
    || pathname === "/v1/client-diagnostics"
    || pathname === "/v1/connections"
    || pathname === "/v1/connections/disconnect"
    || pathname === "/v1/egress"
    || pathname === "/v1/mercator/jobs"
    || pathname === "/v1/connect/auth"
    || pathname.startsWith("/v1/connect/auth/")
    || pathname.startsWith("/v1/access-keys/")
    || pathname.startsWith("/v1/grants/")
    || pathname === "/v1/agent/account-info";
}

type LocalConnectApiFetcher = Readonly<{
  fetch(request: Request): Promise<Response>;
}>;
