import { routeManaged } from "./managedProxy.ts";

type Env = {
  NANOCODEX_BACKEND: Fetcher;
};

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/health") {
      let backend: Response;
      try {
        const backendUrl = new URL("/health", url);
        backend = await env.NANOCODEX_BACKEND.fetch(new Request(backendUrl, {
          headers: { accept: "application/json" },
        }));
      } catch {
        return unavailable();
      }
      let body: unknown;
      try {
        body = await backend.json();
      } catch {
        return unavailable();
      }
      if (!backend.ok
        || !body
        || typeof body !== "object"
        || (body as { status?: unknown }).status !== "ok"
        || (body as { service?: unknown }).service !== "nanocodex") {
        return unavailable();
      }
      return Response.json({ status: "ok", service: "nanocodex-managed-proxy" }, {
        headers: { "cache-control": "no-store" },
      });
    }
    return await routeManaged(request, env, url)
      ?? Response.json({ error: "not_found" }, {
        status: 404,
        headers: { "cache-control": "no-store" },
      });
  },
} satisfies ExportedHandler<Env>;

function unavailable(): Response {
  return Response.json({ status: "unavailable", service: "nanocodex-managed-proxy" }, {
    status: 503,
    headers: { "cache-control": "no-store" },
  });
}
