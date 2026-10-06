import { accountNavigationLinks } from "nanocodex-connect-protocol";

/** Public navigation only. Authentication happens normally in the destination browser. */
export function routeAccountNavigation(request: Request, url: URL): Response | undefined {
  if (url.pathname !== "/v1/account/links") return undefined;
  const headers = { "cache-control": "no-store" };
  if (request.method !== "GET") {
    return Response.json({ error: "method_not_allowed" }, { status: 405, headers: { ...headers, allow: "GET" } });
  }
  // Production APIs and service bindings do not host account documents. Local
  // account development uses the public request URL, never forwarding headers.
  const local = ["localhost", "127.0.0.1", "[::1]", "nanocodex.localhost"].includes(url.hostname)
    || /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/.test(url.hostname);
  const origin = local ? url.origin : "https://nanocodex.gakonst.workers.dev";
  const links = accountNavigationLinks(origin, url.searchParams);
  return Response.json(links ?? { error: "invalid_request" }, { status: links ? 200 : 400, headers });
}
