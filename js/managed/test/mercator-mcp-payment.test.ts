import { Challenge, Credential, Mcp } from "mppx";
import { describe, expect, it, vi } from "vitest";
import { createMcpRuntime } from "../../nanocodex/runtime/mcp-runtime.mjs";
import { mcpPayment } from "nanocodex/tempo";
import type { ToolContext } from "nanocodex";
import { mercatorMcpPayment } from "../src/mercator-mcp-payment";
const context: ToolContext = { callId: "synthetic-call", parentCallId: "", sessionId: "synthetic-session", model: "claude-sonnet-4-6", signal: new AbortController().signal };
function tool(runtime: Awaited<ReturnType<typeof createMcpRuntime>>, name: string) {
  const value = runtime.resolve(name); if (!value) throw Error("fixture tool missing " + name); return value;
}
function content(result: unknown) {
  if (!result || typeof result !== "object" || !("value" in result)) throw Error("invalid fixture tool result");
  const value = result.value as { content: Array<{ text: string }> }; return value.content[0]?.text ?? "";
}
const plan = { nodes: [{ id: "one", serviceId: "synthetic", method: "GET", path: "/lookup" }] };
const args = { idempotency_key: "synthetic-key-123", plan, approved_total: "0.05" };
const challenge = Challenge.from({ id: "synthetic-mcp-challenge", method: "tempo", intent: "charge", realm: "mercator.sh",
  expires: new Date(Date.now() + 300_000).toISOString(), request: { amount: "50000", currency: "0x20c000000000000000000000b9537d11c60e8b50",
    recipient: "0x0000000000000000000000000000000000000002", methodDetails: { chainId: 4217, feePayer: true, supportedModes: ["pull"], machineTokenEnabled: true } },
});
describe("default Mercator MCP wallet payment", () => {
  it("keeps free tools on the normal MCP and retries only create_job with broker-signed metadata", async () => {
    const calls: any[] = [];
    const client = { async listTools() { return { tools: [
      { name: "quote_plan", inputSchema: { type: "object" } }, { name: "create_job", inputSchema: { type: "object" } },
    ] }; }, async callTool(params: any) { calls.push(params);
      if (params.name === "quote_plan") return { content: [{ type: "text", text: JSON.stringify({ totalAmount: "0.05", validUntil: new Date(Date.now() + 60_000).toISOString() }) }] };
      if (!params._meta?.[Mcp.credentialMetaKey]) return { content: [], _meta: { [Mcp.paymentRequiredMetaKey]: { challenges: [challenge] } } };
      expect(params._meta[Mcp.credentialMetaKey]).toMatchObject({ payload: { type: "transaction" } });
      return { content: [{ type: "text", text: "job created" }] };
    } };
    const broker = { fetch: vi.fn(async (_url: string, init: RequestInit) => {
      expect(JSON.parse(String(init.body))).toMatchObject({ ...args, challenge: { id: challenge.id } });
      return Response.json({ credential: Credential.serialize({ challenge, payload: { type: "transaction", signature: "0xsynthetic" } }) });
    }) };
    const payment = mcpPayment(mercatorMcpPayment(broker as never, "owner", () => {}));
    const runtime = await createMcpRuntime({ mercator: { client, payment } });
    try {
      await runtime.settled();
      expect(content(await tool(runtime, "mcp__mercator__quote_plan").handler({ plan }, context))).toContain("totalAmount");
      expect(broker.fetch).not.toHaveBeenCalled();
      const result = await tool(runtime, "mcp__mercator__create_job").handler(args, context);
      expect(content(result)).toBe("job created");
      expect(calls.map(c => c.name)).toEqual(["quote_plan", "quote_plan", "create_job", "create_job"]);
      expect(broker.fetch).toHaveBeenCalledTimes(1);
    } finally { await runtime.close(); }
  // This journey includes the cold Workers module load now deferred to tool use.
  }, 30_000);
  it("rejects a stale or mismatched quote before a paid MCP call", async () => {
    const callTool = vi.fn(async (_params: any) => ({ content: [{ type: "text", text: JSON.stringify({ totalAmount: "0.04",
      validUntil: new Date(Date.now() + 60_000).toISOString() }) }] }));
    const broker = { fetch: vi.fn(async () => Response.json({})) };
    const runtime = await createMcpRuntime({ mercator: { client: {
      async listTools() { return { tools: [{ name: "create_job", inputSchema: { type: "object" } }] }; }, callTool,
    }, payment: mcpPayment(mercatorMcpPayment(broker as never, "owner", () => {})) } });
    try { await runtime.settled();
      await expect(tool(runtime, "mcp__mercator__create_job").handler(args, context)).rejects.toThrow(/quote/);
      expect(callTool).toHaveBeenCalledTimes(1);
      expect(callTool.mock.calls[0][0].name).toBe("quote_plan");
      expect(broker.fetch).not.toHaveBeenCalled();
    } finally { await runtime.close(); }
  });
  it("denies restricted grant execution before contacting the MCP or broker", async () => {
    const callTool = vi.fn(async () => ({ content: [] }));
    const broker = { fetch: vi.fn(async () => Response.json({})) };
    const runtime = await createMcpRuntime({ mercator: { client: {
      async listTools() { return { tools: [{ name: "create_job", inputSchema: { type: "object" } }] }; }, callTool,
    }, payment: mcpPayment(mercatorMcpPayment(broker as never, "owner", () => { throw Error("forbidden"); })) } });
    try { await runtime.settled();
      await expect(tool(runtime, "mcp__mercator__create_job").handler(args, context)).rejects.toThrow("forbidden");
      expect(callTool).not.toHaveBeenCalled(); expect(broker.fetch).not.toHaveBeenCalled();
    } finally { await runtime.close(); }
  });
  it("cannot pay a challenged read-only tool or a missing wallet", async () => {
    const client = { async listTools() { return { tools: [{ name: "get_job", inputSchema: { type: "object" } }, { name: "create_job", inputSchema: { type: "object" } }] }; },
      async callTool(params: any) {
        if (params.name === "quote_plan") return { content: [{ type: "text", text: JSON.stringify({ totalAmount: "0.05", validUntil: new Date(Date.now() + 60_000).toISOString() }) }] };
        return { content: [], _meta: { [Mcp.paymentRequiredMetaKey]: { challenges: [challenge] } } };
      } };
    const broker = { fetch: vi.fn(async () => new Response(null, { status: 404 })) };
    const runtime = await createMcpRuntime({ mercator: { client, payment: mcpPayment(mercatorMcpPayment(broker as never, "owner", () => {})) } });
    try { await runtime.settled();
      await expect(tool(runtime, "mcp__mercator__get_job").handler({ job_id: "test" }, context)).rejects.toThrow();
      expect(broker.fetch).not.toHaveBeenCalled();
      await expect(tool(runtime, "mcp__mercator__create_job").handler(args, context)).rejects.toThrow(/wallet is not configured/);
      expect(broker.fetch).toHaveBeenCalledTimes(1);
    } finally { await runtime.close(); }
  });
  it.each(["cancel_quote", "revoke_quote", "revoke_challenge", "revoke_credential"])(
    "stops a paid call after %s without a credential retry", async (stage) => {
      const controller = new AbortController();
      let allowed = true;
      const calls: any[] = [];
      const client = { async listTools() { return { tools: [{ name: "create_job", inputSchema: { type: "object" } }] }; },
        async callTool(params: any, _schema: unknown, options: any) {
          calls.push(params);
          if (params.name === "quote_plan") {
            expect(options.signal).toBeInstanceOf(AbortSignal);
            if (stage === "cancel_quote") controller.abort();
            if (stage === "revoke_quote") allowed = false;
            return { structuredContent: { totalAmount: "0.05", validUntil: new Date(Date.now() + 60_000).toISOString() }, content: [] };
          }
          if (stage === "revoke_challenge") allowed = false;
          return { content: [], _meta: { [Mcp.paymentRequiredMetaKey]: { challenges: [challenge] } } };
        } };
      const broker = { fetch: vi.fn(async () => {
        allowed = false;
        return Response.json({ credential: Credential.serialize({ challenge, payload: { type: "transaction", signature: "0xsynthetic" } }) });
      }) };
      const runtime = await createMcpRuntime({ mercator: { client, payment: mercatorMcpPayment(broker as never, "owner", (call: any) => {
        expect(call.sessionId).toBe(context.sessionId);
        if (!allowed) throw Error("forbidden");
      }) } });
      try {
        await runtime.settled();
        await expect(tool(runtime, "mcp__mercator__create_job").handler(args, { ...context, signal: controller.signal }))
          .rejects.toThrow(stage === "cancel_quote" ? /cancelled/ : /forbidden/);
        // Let an uncooperative quote settle after cancellation before inspecting effects.
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(calls.map(call => call.name)).toEqual(stage.endsWith("quote") ? ["quote_plan"] : ["quote_plan", "create_job"]);
        expect(calls.some(call => call._meta?.[Mcp.credentialMetaKey])).toBe(false);
        expect(broker.fetch).toHaveBeenCalledTimes(stage === "revoke_credential" ? 1 : 0);
      } finally { await runtime.close(); }
    });

});
