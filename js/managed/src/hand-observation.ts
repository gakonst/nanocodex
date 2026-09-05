import { HandObservationError, type HostedToolsBrokerCore, type HostedObservationSurface } from "nanocodex-tools/hosted";

const headers = { "cache-control": "no-store", "x-content-type-options": "nosniff" };
const json = (body: unknown, status = 200) => Response.json(body, { status, headers });

/** Called only after the owning service has authorized the account. */
export async function handObservationResponse(broker: HostedToolsBrokerCore, request: Request): Promise<Response> {
  const url = new URL(request.url);
  if (request.method !== "GET") return json({ error: "method_not_allowed" }, 405);
  if (url.pathname === "/hands" && !url.search) return json({ surfaces: broker.observationSurfaces() });
  const route = url.searchParams.get("route_token");
  const surface = url.searchParams.get("surface_id");
  if (url.pathname !== "/hands/frame" || [...url.searchParams].length !== 2
    || url.searchParams.getAll("route_token").length !== 1 || url.searchParams.getAll("surface_id").length !== 1
    || !route || route.length > 256 || !surface || surface.length > 128) return json({ error: "invalid_request" }, 400);
  try { return json(await broker.observe(route, surface, request.signal)); }
  catch (error) {
    if (!(error instanceof HandObservationError)) throw error;
    return Response.json({ error: `observation_${error.code}` }, {
      status: error.code === "busy" ? 429 : error.code === "timeout" ? 504 : 409,
      headers: { ...headers, ...(error.code === "busy" ? { "retry-after": "1" } : {}) },
    });
  }
}

export async function sessionHandObservationResponse(options: {
  broker: HostedToolsBrokerCore; account: Fetcher; ownerId: string; request: Request;
}): Promise<Response> {
  const { broker, account, ownerId, request } = options;
  const url = new URL(request.url);
  const accountRequest = () => account.fetch(`https://account-tools.internal${url.pathname}${url.search}`, {
    headers: { "x-nanocodex-owner-id": ownerId }, signal: request.signal,
  });
  if (url.pathname === "/hands" && !url.search) {
    const response = await accountRequest();
    if (!response.ok && response.status !== 404) return response;
    const accountSurfaces = response.ok ? (await response.json<{ surfaces: HostedObservationSurface[] }>()).surfaces : [];
    return json({ surfaces: [
      ...broker.observationSurfaces().map((surface) => ({ ...surface, source: "agent" })),
      ...accountSurfaces.map((surface) => ({ ...surface, source: "account" })),
    ] });
  }
  const source = url.searchParams.get("source");
  if (url.pathname !== "/hands/frame" || url.searchParams.getAll("source").length !== 1
    || (source !== "agent" && source !== "account")) return json({ error: "invalid_request" }, 400);
  url.searchParams.delete("source");
  return source === "account" ? accountRequest() : handObservationResponse(broker, new Request(url, { signal: request.signal }));
}
