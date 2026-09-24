import { describe, expect, it, vi } from "vitest";
import {
  connectedManagedAccountMcps,
  managedAccountMcpServerName,
  managedAccountMcpServers,
} from "../src/default-mcp";

describe("durable managed default MCP catalog", () => {
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
});
