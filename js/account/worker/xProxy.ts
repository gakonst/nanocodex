import { limitAgentOperation, type PublicSecurityEnv } from "./publicSecurity.ts";

export type XProxyEnv = PublicSecurityEnv & { NANOCODEX_X?: Pick<Fetcher, "fetch"> };

/** Browser chats use the same private X Worker as durable agents. */
export async function proxyX(
  request: Request,
  env: XProxyEnv,
  sameOrigin: boolean,
): Promise<Response | undefined> {
  const url = new URL(request.url);
  if (!url.pathname.startsWith("/api/tools/x/")) return undefined;
  if (!sameOrigin) return error("forbidden", 403);
  if (!["/api/tools/x/browse", "/api/tools/x/convert"].includes(url.pathname)) {
    return error("not_found", 404);
  }
  if (request.method !== "GET" && request.method !== "HEAD") {
    return error("method_not_allowed", 405, { allow: "GET, HEAD" });
  }
  if (!env.NANOCODEX_X) return error("X service unavailable", 503);
  const limited = await limitAgentOperation(
    env, `x:${request.headers.get("cf-connecting-ip") ?? "unknown-ip"}`, "search",
  );
  if (limited) return limited;
  const upstream = new URL(url.pathname.replace("/api/tools/x/", "/api/"), "https://x.internal");
  upstream.search = url.search;
  try {
    // Account, provider, and connector credentials never cross this boundary.
    const response = await env.NANOCODEX_X.fetch(new Request(upstream, {
      method: request.method,
      headers: { accept: "application/json" },
      signal: request.signal,
    }));
    const headers = new Headers({ "cache-control": "no-store", "x-content-type-options": "nosniff" });
    for (const name of ["content-type", "retry-after"]) {
      const value = response.headers.get(name);
      if (value !== null) headers.set(name, value);
    }
    return new Response(request.method === "HEAD" ? null : response.body, { status: response.status, headers });
  } catch {
    return error("X service unavailable", 502);
  }
}

function error(message: string, status: number, headers?: Record<string, string>): Response {
  return Response.json({ error: message }, {
    status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff", ...headers },
  });
}
