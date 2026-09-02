import { beforeAll, describe, expect, it, vi } from "vitest";

const MCP_ID = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
const OTHER_MCP_ID = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-abcde";
const HOST_EXCHANGE = "h".repeat(43);

let projectWalletRequest: typeof import("../src/protocol").projectWalletRequest;

beforeAll(async () => {
  vi.stubGlobal("window", {
    addEventListener: vi.fn(),
  });
  ({ projectWalletRequest } = await import("../src/protocol"));
});

function walletRequest(context?: unknown) {
  return projectWalletRequest({
    appId: "atlas-workspace",
    id: "request-id",
    origin: "https://consumer.example",
    rpc: {
      method: "wallet_connect",
      params: [{ capabilities: { auth: { resources: [`urn:nanocodex:mcp:${MCP_ID}`] } } }],
      ...(context === undefined ? {} : { context }),
    },
    type: "walletConnect",
  });
}

describe("Wata wallet request projection", () => {
  it("projects exact secret-free MCP display metadata and focus outside the RPC envelope", () => {
    const projected = walletRequest({
      requestedMcpConnections: [
        { id: MCP_ID, name: "Linear workspace", status: "authorization_required" },
      ],
      focusMcpConnection: MCP_ID,
    });

    expect(projected.requestedMcpConnections).toEqual([
      { id: MCP_ID, name: "Linear workspace", status: "authorization_required" },
    ]);
    expect(projected.focusMcpConnection).toBe(MCP_ID);
    expect(projected.rpc).toEqual({
      method: "wallet_connect",
      params: [{ capabilities: { auth: { resources: [`urn:nanocodex:mcp:${MCP_ID}`] } } }],
    });
    expect("context" in projected.rpc).toBe(false);
  });

  it.each([
    [{ id: "short", name: "Linear", status: "authorization_required" }],
    [{ id: MCP_ID, name: "", status: "authorization_required" }],
    [{ id: MCP_ID, name: " Linear", status: "authorization_required" }],
    [{ id: MCP_ID, name: "x".repeat(257), status: "authorization_required" }],
    [{ id: MCP_ID, name: "Linear", status: "connected" }],
    [
      { id: MCP_ID, name: "Linear", status: "authorization_required" },
      { id: MCP_ID, name: "Duplicate", status: "authorization_required" },
    ],
    Array.from({ length: 17 }, (_, index) => ({
      id: `${index.toString().padStart(2, "0")}${"a".repeat(41)}`,
      name: `MCP ${index}`,
      status: "authorization_required",
    })),
  ])("rejects malformed MCP IDs, names, statuses, and duplicates", (requestedMcpConnections) => {
    expect(() => walletRequest({ requestedMcpConnections })).toThrow(/invalid MCP/);
  });

  it.each([
    { id: MCP_ID, name: "Linear", status: "authorization_required", endpoint: "https://mcp.linear.app/mcp" },
    { id: MCP_ID, name: "Linear", status: "authorization_required", token: "provider-secret" },
    { id: MCP_ID, name: "Linear", status: "authorization_required", headers: { authorization: "Bearer secret" } },
    { id: MCP_ID, name: "Linear", status: "authorization_required", providerCredentials: { secret: "value" } },
  ])("rejects credential-bearing or extra MCP descriptor fields", (descriptor) => {
    expect(() => walletRequest({ requestedMcpConnections: [descriptor] })).toThrow(/invalid MCP/);
  });

  it("rejects extra context fields and focus that is not one of the projected connections", () => {
    expect(() => walletRequest({
      requestedMcpConnections: [{ id: MCP_ID, name: "Linear", status: "authorization_required" }],
      token: "provider-secret",
    })).toThrow(/invalid MCP/);
    expect(() => walletRequest({
      requestedMcpConnections: [{ id: MCP_ID, name: "Linear", status: "authorization_required" }],
      focusMcpConnection: OTHER_MCP_ID,
    })).toThrow(/focused MCP/);
  });

  it("keeps wallet requests without MCP context compatible and secret-free", () => {
    const projected = walletRequest();

    expect(projected.requestedMcpConnections).toBeUndefined();
    expect(projected.focusMcpConnection).toBeUndefined();
    expect(projected.rpc).toEqual({
      method: "wallet_connect",
      params: [{ capabilities: { auth: { resources: [`urn:nanocodex:mcp:${MCP_ID}`] } } }],
    });
  });

  it("projects one resource-bound host exchange without retaining wallet context", () => {
    const expiresAt = Math.floor(Date.now() / 1_000) + 120;
    const projected = projectWalletRequest({
      appId: "atlas-workspace",
      id: "host-request",
      origin: "https://consumer.example",
      rpc: {
        method: "wallet_connect",
        params: [{ capabilities: { auth: { resources: [
          `urn:nanocodex:host-principal:exchange:${HOST_EXCHANGE}`,
        ] } } }],
        context: { hostPrincipal: { token: HOST_EXCHANGE, expiresAt } },
      },
      type: "walletConnect",
    });

    expect(projected.hostPrincipalExchange).toBe(HOST_EXCHANGE);
    expect("context" in projected.rpc).toBe(false);
  });

  it("rejects missing, duplicate, malformed, and mismatched host exchanges", () => {
    const expiresAt = Math.floor(Date.now() / 1_000) + 120;
    expect(() => walletRequest({ hostPrincipal: { token: HOST_EXCHANGE, expiresAt } })).toThrow(/host principal/);
    expect(() => projectWalletRequest({
      appId: "atlas-workspace",
      id: "host-request",
      origin: "https://consumer.example",
      rpc: {
        method: "wallet_connect",
        params: [{ capabilities: { auth: { resources: [
          `urn:nanocodex:host-principal:exchange:${HOST_EXCHANGE}`,
        ] } } }],
      },
      type: "walletConnect",
    })).toThrow(/host principal/);
    for (const resources of [
      [`urn:nanocodex:host-principal:exchange:short`],
      [
        `urn:nanocodex:host-principal:exchange:${HOST_EXCHANGE}`,
        `urn:nanocodex:host-principal:exchange:${"x".repeat(43)}`,
      ],
    ]) {
      expect(() => projectWalletRequest({
        appId: "atlas-workspace",
        id: "host-request",
        origin: "https://consumer.example",
        rpc: { method: "wallet_connect", params: [{ capabilities: { auth: { resources } } }] },
        type: "walletConnect",
      })).toThrow(/host principal/);
    }
  });
});
