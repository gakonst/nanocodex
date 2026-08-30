import { beforeAll, describe, expect, it, vi } from "vitest";

const MCP_ID = "abcdefghijklmnopqrstuvwxyz0123456789_-ABCDE";
const OTHER_MCP_ID = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-abcde";

let projectWalletRequest: typeof import("../src/protocol").projectWalletRequest;
let parentDialog: typeof import("../src/protocol").parentDialog;
let messageListener: (event: MessageEvent<unknown>) => void;
const parent = { postMessage: vi.fn() };

beforeAll(async () => {
  vi.stubGlobal("window", {
    parent,
    addEventListener: vi.fn((type, listener) => {
      if (type === "message") messageListener = listener;
    }),
  });
  ({ parentDialog, projectWalletRequest } = await import("../src/protocol"));
});

describe("embedded WebMCP approval protocol", () => {
  it("binds the request to its parent origin and waits for execution completion", async () => {
    const request = {
      id: "webmcp-approval",
      type: "webMcpApproval",
      app: { id: "webmcp:consumer.example", name: "Sodium", origin: "https://consumer.example" },
      action: {
        kind: "webmcp",
        name: "send_transfer",
        input: { amount: "25.00", to: "Ada" },
        readOnly: false,
      },
    } as const;
    messageListener({
      data: { type: "nanocodex:request", id: request.id, request },
      origin: "https://consumer.example",
      source: parent,
    } as unknown as MessageEvent);
    expect(parentDialog.getRequest()).toEqual(request);

    const completion = parentDialog.approve();
    expect(parent.postMessage).toHaveBeenLastCalledWith({
      type: "nanocodex:approval",
      id: request.id,
    }, "https://consumer.example");
    messageListener({
      data: { type: "nanocodex:completion", id: request.id, ok: true },
      origin: "https://consumer.example",
      source: parent,
    } as unknown as MessageEvent);
    await completion;
    await parentDialog.respond({ approved: true });
    expect(parent.postMessage).toHaveBeenLastCalledWith({
      type: "nanocodex:response",
      id: request.id,
      result: { approved: true },
    }, "https://consumer.example");
    expect(parentDialog.getRequest()).toBeUndefined();
  });

  it("rejects a WebMCP request that lies about its embedding origin", () => {
    const request = {
      id: "origin-substitution",
      type: "webMcpApproval",
      app: { id: "webmcp:attacker.example", name: "Attacker", origin: "https://attacker.example" },
      action: { kind: "webmcp", name: "send_transfer", input: {}, readOnly: false },
    } as const;
    messageListener({
      data: { type: "nanocodex:request", id: request.id, request },
      origin: "https://consumer.example",
      source: parent,
    } as unknown as MessageEvent);
    expect(parentDialog.getRequest()).toBeUndefined();
  });
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
});
