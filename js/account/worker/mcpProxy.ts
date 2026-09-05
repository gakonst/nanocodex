const MCP_UPSTREAMS = Object.freeze({
  "openai-developer-docs": "https://developers.openai.com/mcp",
  cloudflare: "https://docs.mcp.cloudflare.com/mcp",
  viem: "https://viem.sh/api/mcp",
  vocs: "https://vocs.dev/api/mcp",
});

const REQUEST_HEADERS = [
  "accept",
  "content-type",
  "last-event-id",
  "mcp-protocol-version",
  "mcp-session-id",
];
const RESPONSE_HEADERS = [
  "cache-control",
  "content-type",
  "mcp-protocol-version",
  "mcp-session-id",
  "retry-after",
  "www-authenticate",
];
const METHODS = new Set(["GET", "POST", "DELETE"]);
const THREAD_ID = /^[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;

export async function proxyDefaultMcp(
  request: Request,
  url: URL,
  authorized: boolean,
  egress?: Fetcher,
): Promise<Response | undefined> {
  const match = /^\/api\/mcp\/([a-z-]+)$/.exec(url.pathname);
  if (!match) return undefined;
  const upstreamBase = MCP_UPSTREAMS[match[1] as keyof typeof MCP_UPSTREAMS];
  if (!upstreamBase) return error("unknown MCP server", 404);
  if (!authorized) return error("forbidden", 403);
  if (!METHODS.has(request.method)) {
    return error("method not allowed", 405, { allow: "GET, POST, DELETE" });
  }

  const threadId = url.searchParams.get("thread_id");
  if (!threadId || !THREAD_ID.test(threadId)) return error("invalid thread", 400);
  if (!egress) return error("egress unavailable", 503);

  const upstream = new URL(upstreamBase);
  const upstreamQuery = new URLSearchParams(url.searchParams);
  upstreamQuery.delete("thread_id");
  upstream.search = upstreamQuery.toString();
  const headers = new Headers();
  for (const name of REQUEST_HEADERS) {
    const value = request.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  const gatewayHeaders = new Headers({
    "content-type": "application/json",
    origin: url.origin,
  });
  const cookie = request.headers.get("cookie");
  if (cookie !== null) gatewayHeaders.set("cookie", cookie);
  const response = await egress.fetch(new Request(new URL("/v1/egress", url.origin), {
    method: "POST",
    headers: gatewayHeaders,
    body: JSON.stringify({
      thread_id: threadId,
      url: upstream.href,
      method: request.method,
      headers: Object.fromEntries(headers.entries()),
      ...(request.method === "GET" || request.body === null
        ? {}
        : { body: await request.text() }),
    }),
    signal: request.signal,
  }));
  const responseHeaders = new Headers({
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  for (const name of RESPONSE_HEADERS) {
    const value = response.headers.get(name);
    if (value !== null) responseHeaders.set(name, value);
  }
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: responseHeaders,
  });
}

function error(message: string, status: number, headers?: HeadersInit): Response {
  return Response.json({ error: message }, {
    status,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...headers,
    },
  });
}
