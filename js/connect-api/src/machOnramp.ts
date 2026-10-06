export type MachOnrampEnv = Readonly<{
  MACH_ONRAMP?: { fetch(request: Request): Promise<Response> };
  MACH_ONRAMP_RELAY_URL?: string;
  MACH_ONRAMP_RELAY_TOKEN?: string;
}>;

export const relayTokenHeader = "x-nanocodex-mach-relay-token";
const orderPath = /^\/v1\/orders\/ord_[0-9a-f]{32}$/;

export function isMachRoute(method: string, path: string): boolean {
  return (method === "GET" && (path === "/v1/config" || orderPath.test(path)))
    || (method === "POST" && path === "/v1/orders");
}

/** Only the MACH config/order contract crosses this boundary. Never forward cookies. */
export async function machOnramp(request: Request, env: MachOnrampEnv, path: string): Promise<Response> {
  if (!isMachRoute(request.method, path)) return failure(404, "not_found");
  const headers = new Headers({ accept: "application/json" });
  for (const name of ["content-type", "authorization", "idempotency-key"]) {
    const value = request.headers.get(name);
    if (value) headers.set(name, value);
  }
  try {
    let origin = "https://mach.internal";
    if (!env.MACH_ONRAMP) {
      if (!env.MACH_ONRAMP_RELAY_URL || !env.MACH_ONRAMP_RELAY_TOKEN
        || env.MACH_ONRAMP_RELAY_TOKEN.length < 32) return failure(503, "machine_usd_unavailable");
      const relay = new URL(env.MACH_ONRAMP_RELAY_URL);
      if (relay.protocol !== "https:" || relay.username || relay.password
        || relay.pathname !== "/" || relay.search || relay.hash) return failure(503, "machine_usd_unavailable");
      origin = relay.origin;
      headers.set(relayTokenHeader, env.MACH_ONRAMP_RELAY_TOKEN);
    }
    const upstream = new Request(new URL(path, origin), {
      method: request.method,
      headers,
      body: request.method === "POST" ? await request.text() : undefined,
      redirect: "manual",
    });
    const response = env.MACH_ONRAMP ? await env.MACH_ONRAMP.fetch(upstream) : await fetch(upstream);
    // Never follow provider redirects or expose their cookies/headers to a browser.
    if (response.status >= 300 && response.status < 400) {
      await response.body?.cancel();
      return failure(502, "machine_usd_upstream_error");
    }
    const responseHeaders = new Headers({
      "content-type": response.headers.get("content-type") ?? "application/json",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    });
    const retryAfter = response.headers.get("retry-after");
    if (retryAfter) responseHeaders.set("retry-after", retryAfter);
    return new Response(response.body, { status: response.status, headers: responseHeaders });
  } catch {
    // The create may have succeeded. Callers must retain their idempotency key.
    return failure(502, "machine_usd_outcome_unknown");
  }
}

export function failure(status: number, error: string): Response {
  return Response.json({ error }, { status, headers: { "cache-control": "no-store" } });
}
