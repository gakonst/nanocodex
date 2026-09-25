import { describe, expect, it, vi } from "vitest";
import { relayChatGpt, routeChatGpt } from "../src/relay";
import { createEgressHandler, CREDENTIAL_PLACEHOLDER } from "../src/handler";

describe("subscription relay placement", () => {
  it("prefers the VPC Gateway and passes a WebSocket upgrade through without invoking the relay DO", async () => {
    const switched = { status: 101, webSocket: { accept() {} } } as unknown as Response;
    const gatewayFetch = vi.fn(async (request: Request) => {
      expect(request.url).toBe("https://chatgpt.com/backend-api/codex/responses");
      expect(request.method).toBe("GET");
      expect(request.headers.get("authorization")).toBe("Bearer synthetic-token");
      expect(request.headers.get("upgrade")).toBe("websocket");
      expect(request.headers.get("chatgpt-account-id")).toBe("synthetic-account");
      expect(request.headers.has("x-managed2-owner")).toBe(false);
      return switched;
    });
    const relayGet = vi.fn(() => { throw new Error("relay should not be used"); });
    const bindings = {
      GATEWAY: { fetch: gatewayFetch } as unknown as Fetcher,
      CHATGPT_EGRESS: { get: relayGet } as unknown as DurableObjectNamespace,
    };
    const handler = createEgressHandler({
      readCredential: async () => ({ kind: "chatgpt" as const, secret: "synthetic-token",
        accountId: "synthetic-account", expiresAt: Date.now() + 3_600_000, fedramp: false }),
      upstreamFetch: (request, ownerId) => routeChatGpt(request, ownerId, bindings),
    });
    const response = await handler.fetch(new Request("https://api.openai.com/v1/responses", {
      headers: { authorization: `Bearer ${CREDENTIAL_PLACEHOLDER}`, upgrade: "websocket",
        "x-managed2-owner": "owner-1" },
    }), {});
    expect(response).toBe(switched);
    expect(relayGet).not.toHaveBeenCalled();
    expect(gatewayFetch).toHaveBeenCalledOnce();
  });

  it("routes the exact Responses request through the owner's existing private container", async () => {
    const fetch = vi.fn(async (request: Request) => {
      expect(request.url).toBe("https://chatgpt-egress.internal/backend-api/codex/responses");
      expect(request.method).toBe("POST");
      expect(request.headers.get("authorization")).toBe("Bearer synthetic-token");
      expect(await request.text()).toBe("{}");
      return new Response("ok");
    });
    const namespace = { idFromName: vi.fn((id: string) => id), get: vi.fn(() => ({ fetch })) };
    const request = new Request("https://chatgpt.com/backend-api/codex/responses", {
      method: "POST", headers: { authorization: "Bearer synthetic-token" }, body: "{}",
    });
    const response = await routeChatGpt(request, "owner-1", {
      CHATGPT_EGRESS: namespace as unknown as DurableObjectNamespace,
    });
    expect(await response.text()).toBe("ok");
    expect(namespace.idFromName).toHaveBeenCalledWith("user-v1:owner-1");
  });

  it("reports ChatGPT subscription route only after passing through the existing Codex container", async () => {
    let tick = 0;
    const events: Record<string, string | number | null>[] = [];
    const containerFetch = vi.fn(async (out: Request) => {
      expect(out.url).toBe("https://chatgpt-egress.internal/backend-api/codex/responses");
      expect(out.headers.get("chatgpt-account-id")).toBe("synthetic-account");
      expect(out.headers.get("originator")).toBe("codex_cli_rs");
      tick += 12;
      return new Response("event: ok\n\n", { headers: { "content-type": "text/event-stream" } });
    });
    const namespace = { idFromName: vi.fn((id: string) => id), get: vi.fn(() => ({ fetch: containerFetch })) };
    const handler = createEgressHandler({
      readCredential: async () => { tick += 5; return { kind: "chatgpt" as const, secret: "synthetic-access",
        accountId: "synthetic-account", expiresAt: Date.now() + 3_600_000, fedramp: false }; },
      upstreamFetch: (out, owner) => relayChatGpt(out, owner, namespace as unknown as DurableObjectNamespace),
      clock: () => tick, log: event => events.push(event),
    });
    const response = await handler.fetch(new Request("https://api.openai.com/v1/responses", {
      method: "POST", headers: { "x-managed2-owner": "owner-synthetic",
        authorization: `Bearer ${CREDENTIAL_PLACEHOLDER}` }, body: "{}",
    }), {});
    expect(await response.text()).toBe("event: ok\n\n");
    expect(response.headers.get("server-timing")).toContain('egress_route;desc="chatgpt_subscription"');
    expect(response.headers.get("server-timing")).toContain("egress_upstream_headers;dur=12.0");
    expect(events[0]).toMatchObject({ route_kind: "chatgpt_subscription", upstream_status: 200,
      credential_cache: "miss", credential_ms: 5, upstream_headers_ms: 12 });
    expect(namespace.idFromName).toHaveBeenCalledWith("user-v1:owner-synthetic");
    expect(containerFetch).toHaveBeenCalledOnce();
    expect(JSON.stringify(events)).not.toMatch(/owner-synthetic|synthetic-account|synthetic-access/);
  });
});
