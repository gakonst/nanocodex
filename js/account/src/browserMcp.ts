export const DEFAULT_BROWSER_MCP_SERVERS = Object.freeze({
  openaiDeveloperDocs: {
    path: "/api/mcp/openai-developer-docs",
    description: "Search OpenAI developer documentation.",
    enabledTools: ["search_openai_docs"],
  },
  cloudflare: {
    path: "/api/mcp/cloudflare",
    description: "Search Cloudflare developer documentation.",
    enabledTools: ["search_cloudflare_documentation"],
  },
  viem: {
    path: "/api/mcp/viem",
    description: "Search Viem developer documentation.",
    enabledTools: ["search_docs"],
  },
  vocs: {
    path: "/api/mcp/vocs",
    description: "Search Vocs developer documentation.",
    enabledTools: ["search_docs"],
  },
});

export type BrowserAccountMcpConnection = Readonly<{
  id: string;
  name: string;
}>;

export const ACCOUNT_MCP_CATALOG_CHANGED = "nanocodex:account-mcp-catalog-changed";

export function announceAccountMcpCatalogChanged(): void {
  window.dispatchEvent(new Event(ACCOUNT_MCP_CATALOG_CHANGED));
}

const MCP_CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const MCP_CONNECTION_NAME = /^[^\u0000-\u001f\u007f]{1,256}$/u;
const ACCOUNT_MCP_LIST_TIMEOUT_MS = 10_000;

export function browserMcpConfiguration(
  origin: string,
  threadId: string,
  accountConnections: readonly BrowserAccountMcpConnection[] = [],
) {
  return Object.fromEntries([
    ...Object.entries(DEFAULT_BROWSER_MCP_SERVERS).map(([name, server]) => [
      name,
      {
        description: server.description,
        enabledTools: [...server.enabledTools],
        headers: { "x-nanocodex-request": "1" },
        startupTimeoutMs: 30_000,
        timeoutMs: 300_000,
        url: mcpUrl(server.path, origin, threadId),
      },
    ]),
    ...accountConnections.map((connection) => [
      accountMcpServerName(connection),
      {
        description: `${connection.name} · connected account MCP`,
        headers: { "x-nanocodex-request": "1" },
        startupTimeoutMs: 30_000,
        timeoutMs: 300_000,
        url: mcpUrl(
          `/v1/connectors/mcp-connections/${encodeURIComponent(connection.id)}/proxy`,
          origin,
          threadId,
        ),
      },
    ]),
  ]);
}

export async function loadBrowserAccountMcpConnections(
  signal?: AbortSignal,
): Promise<readonly BrowserAccountMcpConnection[]> {
  const controller = new AbortController();
  const abort = () => controller.abort(signal?.reason);
  if (signal?.aborted) abort();
  else signal?.addEventListener("abort", abort, { once: true });
  const timeout = setTimeout(() => {
    controller.abort(new DOMException("account MCP listing timed out", "TimeoutError"));
  }, ACCOUNT_MCP_LIST_TIMEOUT_MS);
  try {
    const response = await fetch("/v1/connectors/mcp-connections", {
      headers: { "x-nanocodex-request": "1" },
      signal: controller.signal,
    });
    if (response.status === 401) {
      await response.body?.cancel();
      return [];
    }
    if (!response.ok) {
      await response.body?.cancel();
      throw new Error(`account MCP listing failed with HTTP ${response.status}`);
    }
    const value: unknown = await response.json();
    if (!isRecord(value) || !Array.isArray(value.mcp_connections)
      || value.mcp_connections.length > 64) {
      throw new Error("account MCP listing returned an invalid response");
    }
    const seen = new Set<string>();
    const connected: BrowserAccountMcpConnection[] = [];
    for (const candidate of value.mcp_connections) {
      if (!isRecord(candidate)
        || typeof candidate.id !== "string" || !MCP_CONNECTION_ID.test(candidate.id)
        || typeof candidate.name !== "string" || !MCP_CONNECTION_NAME.test(candidate.name)
        || candidate.name.trim().length === 0
        || typeof candidate.status !== "string"
        || !["authorization_required", "connected", "reauthorization_required", "disabled"].includes(candidate.status)
        || seen.has(candidate.id)) {
        throw new Error("account MCP listing returned an invalid response");
      }
      seen.add(candidate.id);
      if (candidate.status === "connected") {
        connected.push(Object.freeze({ id: candidate.id, name: candidate.name }));
      }
    }
    return Object.freeze(connected);
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", abort);
  }
}

function accountMcpServerName(connection: BrowserAccountMcpConnection): string {
  return `account_${connection.id}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function mcpUrl(path: string, origin: string, threadId: string): string {
  const url = new URL(path, origin);
  url.searchParams.set("thread_id", threadId);
  return url.href;
}
