import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_MANAGED_MCP_CATALOG,
  connectedManagedAccountMcps,
  createDefaultManagedTools,
  defaultManagedMcpServers,
  managedAccountMcpServerName,
  managedAccountMcpServers,
} from "../src/default-mcp";
import { memorySessionTools } from "../src/memory-session-tools";

describe("durable managed default MCP catalog", () => {
  it("matches the canonical five public MCP servers", () => {
    expect(DEFAULT_MANAGED_MCP_CATALOG).toEqual({
      openaiDeveloperDocs: {
        url: "https://developers.openai.com/mcp",
        description: "Search OpenAI developer documentation.",
        parallelTools: ["fetch_openai_doc", "search_openai_docs"],
      },
      tempo: {
        url: "https://mcp.tempo.xyz",
        description: "Tempo network and protocol tools.",
        parallelTools: ["code", "search"],
      },
      cloudflare: {
        url: "https://docs.mcp.cloudflare.com/mcp",
        description: "Search Cloudflare developer documentation.",
        parallelTools: ["search_cloudflare_documentation"],
      },
      viem: {
        url: "https://viem.sh/api/mcp",
        description: "Search Viem developer documentation.",
        parallelTools: ["list_pages", "read_page", "search_docs", "search_source"],
      },
      vocs: {
        url: "https://vocs.dev/api/mcp",
        description: "Search Vocs developer documentation.",
        parallelTools: ["list_pages", "read_page", "search_docs", "search_source"],
      },
    });
  });

  it("places every default on the managed server fetch boundary", () => {
    const fetcher = vi.fn<typeof fetch>();
    const configured = defaultManagedMcpServers(fetcher);

    expect(Object.keys(configured)).toEqual([
      "openaiDeveloperDocs",
      "tempo",
      "cloudflare",
      "viem",
      "vocs",
    ]);
    for (const server of Object.values(configured)) {
      expect(typeof server).toBe("object");
      expect((server as { fetch?: typeof fetch }).fetch).toBe(fetcher);
    }
  });

  it("strictly selects connected account MCP metadata", async () => {
    const connectedId = "a".repeat(43);
    const broker = {
      fetch: vi.fn(async () => Response.json({
        endpoint: "https://must-not-leak.test/mcp",
        access_token: "must-not-leak",
        mcp_connections: [
          { id: connectedId, name: "Linear workspace", status: "connected" },
          { id: "b".repeat(43), name: "Pending", status: "authorization_required" },
          { id: "c".repeat(43), name: "Revoked", status: "revoked" },
        ],
      })),
    } as unknown as Fetcher;

    await expect(connectedManagedAccountMcps(broker, "user/id")).resolves.toEqual([
      { id: connectedId, name: "Linear workspace" },
    ]);
    expect((broker.fetch as ReturnType<typeof vi.fn>).mock.calls[0]?.[0]).toBe(
      "https://broker.internal/users/user%2Fid/mcp-connections",
    );
  });

  it("binds account MCP transport to one internal connection and live authorization", async () => {
    const connection = { id: "a".repeat(43), name: "Linear workspace" };
    let allowed = false;
    const broker = { fetch: vi.fn(async () => new Response("ok")) } as unknown as Fetcher;
    const configured = managedAccountMcpServers(
      [connection],
      broker,
      "s".repeat(43),
      () => allowed,
    );
    const name = managedAccountMcpServerName(connection);
    const server = configured[name] as {
      fetch: typeof fetch;
      isAvailable: () => boolean;
      url: string;
    };

    expect(server.url).toBe(`https://mcp.internal/v1/connections/${connection.id}`);
    expect(server.isAvailable()).toBe(false);
    await expect(server.fetch(server.url)).rejects.toThrow(/unavailable/);
    expect(broker.fetch).not.toHaveBeenCalled();
    allowed = true;
    expect(server.isAvailable()).toBe(true);
    await expect(server.fetch(`${server.url}?redirect=https://attacker.test`)).rejects.toThrow(
      /escaped its connection boundary/,
    );
    await server.fetch(server.url, {
      method: "POST",
      headers: {
        authorization: "Bearer must-not-forward",
        cookie: "must-not-forward",
        "content-type": "application/json",
        "mcp-session-id": "session-1",
      },
      body: "{}",
    });
    const request = (broker.fetch as ReturnType<typeof vi.fn>).mock.calls[0]?.[0] as Request;
    expect(request.url).toBe(server.url);
    expect(request.headers.get("x-nanocodex-subject")).toBe("s".repeat(43));
    expect(request.headers.get("content-type")).toBe("application/json");
    expect(request.headers.get("mcp-session-id")).toBe("session-1");
    expect(request.headers.has("authorization")).toBe(false);
    expect(request.headers.has("cookie")).toBe(false);
  });

  it("prepares cloud and MCP tools behind one server-owned search catalog", async () => {
    const mcp = Object.fromEntries(
      Object.keys(DEFAULT_MANAGED_MCP_CATALOG).map((server) => [
        server,
        {
          client: {
            async listTools() {
              return {
                tools: [{
                  name: "search",
                  description: `Search ${server}`,
                  inputSchema: { type: "object", additionalProperties: false },
                }],
              };
            },
            async callTool() {
              return { content: [] };
            },
          },
        },
      ]),
    );
    const tools = await createDefaultManagedTools([
      {
        name: "accountInfo",
        description: "Account information.",
        parameters: { type: "object", additionalProperties: false },
        handler: () => ({ ready: true }),
      },
      ...memorySessionTools({
        findSessions: async () => ({ query: "", results: [], citations: [] }),
        readSession: async () => ({ turns: [], citations: [] }),
        memory: async () => ({ operation: "scan", abstained: true, candidates: [] }),
        requireCapability() {},
        requireRootMemoryMutation() {},
        recordCitations() {},
      }),
    ], mcp);
    const socket = new CatalogSocket();
    const connector = tools.attach({
      endpoint: "wss://managed.test/tools",
      transport: { connect: async () => socket },
    });

    try {
      const connecting = connector.connect();
      await waitFor(() => socket.frames.some((frame) => frame.type === "catalog"));
      const catalog = socket.frames.find((frame) => frame.type === "catalog");
      expect(catalog?.tools?.map((entry) => entry.definition.name).sort()).toEqual([
        "accountInfo",
        "find_sessions",
        "memory",
        "mcp__cloudflare__search",
        "mcp__openaiDeveloperDocs__search",
        "mcp__tempo__search",
        "mcp__viem__search",
        "mcp__vocs__search",
        "read_session",
      ].sort());
      socket.receive({ type: "ready" });
      const client = await connecting;
      const closing = client.close();
      await waitFor(() => socket.frames.some((frame) => frame.type === "drain"));
      socket.receive({ type: "draining" });
      await closing;
    } finally {
      await tools.close();
    }
  }, 10_000);
});

type AttachmentFrame = {
  type: string;
  tools?: { definition: { name: string } }[];
};

class CatalogSocket {
  readyState = 1;
  frames: AttachmentFrame[] = [];
  listeners = new Map<string, ((event: { data?: string; code?: number; reason?: string }) => void)[]>();

  send(value: string) {
    this.frames.push(JSON.parse(value) as AttachmentFrame);
  }

  close(code?: number, reason?: string) {
    this.readyState = 3;
    this.emit("close", { code, reason });
  }

  addEventListener(
    type: string,
    listener: (event: { data?: string; code?: number; reason?: string }) => void,
  ) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  receive(frame: AttachmentFrame) {
    this.emit("message", { data: JSON.stringify(frame) });
  }

  emit(type: string, event: { data?: string; code?: number; reason?: string }) {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

async function waitFor(predicate: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  throw new Error("condition did not become true");
}
