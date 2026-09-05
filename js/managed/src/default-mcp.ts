import type { McpServers, NamedTool, Tools } from "nanocodex";
import { createTools } from "nanocodex/tools";
import { fetchResponseWithDeadline } from "./deadline";

export const DEFAULT_MANAGED_MCP_CATALOG = Object.freeze({
  openaiDeveloperDocs: Object.freeze({
    url: "https://developers.openai.com/mcp",
    description: "Search OpenAI developer documentation.",
    parallelTools: Object.freeze(["fetch_openai_doc", "search_openai_docs"]),
  }),
  tempo: Object.freeze({
    url: "https://mcp.tempo.xyz",
    description: "Tempo network and protocol tools.",
    parallelTools: Object.freeze(["code", "search"]),
  }),
  cloudflare: Object.freeze({
    url: "https://docs.mcp.cloudflare.com/mcp",
    description: "Search Cloudflare developer documentation.",
    parallelTools: Object.freeze(["search_cloudflare_documentation"]),
  }),
  viem: Object.freeze({
    url: "https://viem.sh/api/mcp",
    description: "Search Viem developer documentation.",
    parallelTools: Object.freeze(["list_pages", "read_page", "search_docs", "search_source"]),
  }),
  vocs: Object.freeze({
    url: "https://vocs.dev/api/mcp",
    description: "Search Vocs developer documentation.",
    parallelTools: Object.freeze(["list_pages", "read_page", "search_docs", "search_source"]),
  }),
});

export type ManagedAccountMcpConnection = Readonly<{
  id: string;
  name: string;
}>;

const MCP_CONNECTION_ID = /^[A-Za-z0-9_-]{43}$/;
const MCP_CONNECTION_NAME = /^[^\u0000-\u001f\u007f]{1,256}$/u;
const ACCOUNT_MCP_LIST_TIMEOUT_MS = 10_000;
const MCP_REQUEST_HEADERS = [
  "accept",
  "content-type",
  "last-event-id",
  "mcp-protocol-version",
  "mcp-session-id",
] as const;

export function defaultManagedMcpServers(
  fetcher: typeof globalThis.fetch = globalThis.fetch,
): McpServers {
  return Object.fromEntries(
    Object.entries(DEFAULT_MANAGED_MCP_CATALOG).map(([name, server]) => [
      name,
      {
        ...server,
        parallelTools: [...server.parallelTools],
        fetch: fetcher,
      },
    ]),
  );
}

export async function connectedManagedAccountMcps(
  broker: Fetcher,
  userId: string,
): Promise<readonly ManagedAccountMcpConnection[]> {
  return fetchResponseWithDeadline(
    broker,
    `https://broker.internal/users/${encodeURIComponent(userId)}/mcp-connections`,
    {},
    ACCOUNT_MCP_LIST_TIMEOUT_MS,
    "account MCP listing",
    async (response) => {
      if (!response.ok) {
        throw new Error(`account MCP listing failed with HTTP ${response.status}`);
      }
      const value: unknown = await response.json();
      if (!isRecord(value) || !Array.isArray(value.mcp_connections)
        || value.mcp_connections.length > 64) {
        throw new Error("account MCP listing returned an invalid response");
      }
      const seen = new Set<string>();
      const connected: ManagedAccountMcpConnection[] = [];
      for (const candidate of value.mcp_connections) {
        if (!isRecord(candidate)
          || typeof candidate.id !== "string" || !MCP_CONNECTION_ID.test(candidate.id)
          || typeof candidate.name !== "string" || !MCP_CONNECTION_NAME.test(candidate.name)
          || candidate.name.trim().length === 0
          || typeof candidate.status !== "string"
          || !["authorization_required", "connected", "reauthorization_required", "disabled", "revoked"].includes(candidate.status)
          || seen.has(candidate.id)) {
          throw new Error("account MCP listing returned an invalid response");
        }
        seen.add(candidate.id);
        if (candidate.status === "connected") {
          connected.push(Object.freeze({ id: candidate.id, name: candidate.name }));
        }
      }
      return Object.freeze(connected);
    },
  );
}

export function managedAccountMcpServers(
  connections: readonly ManagedAccountMcpConnection[],
  broker: Fetcher,
  subject: string,
  isAvailable: (connectionId: string) => boolean,
): McpServers {
  return Object.fromEntries(connections.map((connection) => {
    const available = () => isAvailable(connection.id);
    return [managedAccountMcpServerName(connection), {
      description: `${connection.name} · connected account MCP`,
      fetch: managedAccountMcpFetch(broker, subject, connection.id, available),
      isAvailable: available,
      startupTimeoutMs: 30_000,
      timeoutMs: 300_000,
      url: `https://mcp.internal/v1/connections/${connection.id}`,
    }];
  }));
}

/** Prepares the cloud-owned tools and public MCP catalog as one durable runtime. */
export function createDefaultManagedTools(
  tools: readonly NamedTool[],
  mcp: McpServers = defaultManagedMcpServers(),
  catalogProvider?: (serverName: string) => string | undefined,
): Promise<Tools> {
  return createTools({
    tools,
    mcp,
    mcpOptions: {
      clientName: "nanocodex-managed",
      clientVersion: "0.5.0",
      ...(catalogProvider === undefined ? {} : { catalogProvider }),
    },
  });
}

function managedAccountMcpFetch(
  broker: Fetcher,
  subject: string,
  connectionId: string,
  isAvailable: () => boolean,
): typeof globalThis.fetch {
  const endpoint = new URL(`https://mcp.internal/v1/connections/${connectionId}`);
  return async (input, init) => {
    if (!isAvailable()) throw new Error("managed account MCP is unavailable");
    const source = input instanceof Request ? new Request(input, init) : new Request(input, init);
    const url = new URL(source.url);
    if (url.origin !== endpoint.origin || url.pathname !== endpoint.pathname
      || url.search || url.hash || url.username || url.password) {
      throw new TypeError("managed account MCP fetch escaped its connection boundary");
    }
    const headers = new Headers();
    for (const name of MCP_REQUEST_HEADERS) {
      const value = source.headers.get(name);
      if (value !== null) headers.set(name, value);
    }
    headers.set("x-nanocodex-subject", subject);
    return broker.fetch(new Request(endpoint, {
      method: source.method,
      headers,
      ...(source.method === "GET" || source.method === "HEAD" || source.body === null
        ? {}
        : { body: source.body }),
      redirect: "manual",
      signal: source.signal,
    }));
  };
}

export function managedAccountMcpServerName(connection: ManagedAccountMcpConnection): string {
  return `account_${connection.id}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
