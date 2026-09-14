import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";

import { ContainerProxy, Sandbox } from "../src/sandbox-runtime";

type Route = { method: string; params: unknown };
type Configuration = {
  outboundByHostOverrides: Record<string, Route>;
  outboundHandlerOverride: Route;
  hasInterceptAllRegistration: boolean;
};

// Run the installed Containers SDK's restoration and setter methods without
// starting a container. Its persisted routing is independent of that process.
function restore(configuration: Configuration) {
  const captured: { props: Record<string, unknown> }[] = [];
  const persisted = vi.fn();
  const runtime = Object.create(Sandbox.prototype) as {
    restoreOutboundConfiguration(): Configuration;
    setOutboundByHost: Sandbox["setOutboundByHost"];
  };
  Object.assign(runtime, {
    ctx: {
      id: { toString: () => "retained-sandbox" },
      storage: { kv: { get: () => structuredClone(configuration), put: persisted } },
      exports: { ContainerProxy: (value: { props: Record<string, unknown> }) => {
        captured.push(value);
        return { fetch: vi.fn() };
      } },
    },
    container: {
      interceptOutboundHttp: vi.fn(),
      interceptOutboundHttps: vi.fn(),
      interceptAllOutboundHttp: vi.fn(),
    },
    usingInterception: true,
    enableInternet: false,
    interceptHttps: true,
  });
  return { runtime, captured, persisted };
}

function configuration() {
  return {
    outboundByHostOverrides: {
      "r2.internal": { method: "r2EgressMount", params: { buckets: {
        NANOCODEX_WORKSPACES: { prefix: `sessions/${crypto.randomUUID()}`, readOnly: false },
        NANOCODEX_WORKSPACES_0: { prefix: `sessions/${crypto.randomUUID()}`, readOnly: true },
      } } },
      "nanocodex-hand.internal": { method: "account", params: { subject: "a".repeat(43), hand: { owner: "owner", id: "hand" } } },
    },
    outboundHandlerOverride: { method: "account", params: { subject: "a".repeat(43) } },
    hasInterceptAllRegistration: true,
  };
}

describe("retained Sandbox R2 outbound restoration", () => {
  it("keeps saved mount permissions through a cold restore and desktop host update", async () => {
    const saved = configuration();
    const { runtime, captured, persisted } = restore(saved);
    expect(runtime.restoreOutboundConfiguration().outboundByHostOverrides).toEqual(saved.outboundByHostOverrides);

    await runtime.setOutboundByHost("nanocodex-hand.internal", "account", {
      subject: "a".repeat(43), hand: { owner: "owner", id: "renewed-hand" },
    });
    const props = captured.at(-1)!.props;
    expect(props.outboundByHostOverrides).toEqual({
      ...saved.outboundByHostOverrides,
      "nanocodex-hand.internal": { method: "account", params: {
        subject: "a".repeat(43), hand: { owner: "owner", id: "renewed-hand" },
      } },
    });
    expect(persisted).toHaveBeenCalled();

    const context = createExecutionContext();
    Object.defineProperty(context, "props", { value: props });
    const bucket = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;
    const proxy = new ContainerProxy(context, {
      NANOCODEX_WORKSPACES: bucket, NANOCODEX_WORKSPACES_0: bucket,
    });
    const request = (binding: string, init?: RequestInit) => proxy.fetch(new Request(
      `http://r2.internal/${binding}/restored.txt`, init,
    ));
    expect((await request("NANOCODEX_WORKSPACES", {
      method: "PUT", body: "retained", headers: { "Content-Length": "8" },
    })).status).toBe(200);
    expect(await (await request("NANOCODEX_WORKSPACES")).text()).toBe("retained");
    const prefix = saved.outboundByHostOverrides["r2.internal"].params.buckets.NANOCODEX_WORKSPACES.prefix;
    expect(await (await bucket.get(`${prefix}/restored.txt`))!.text()).toBe("retained");
    for (const binding of ["NANOCODEX_WORKSPACES_0", "UNMOUNTED"]) {
      expect((await request(binding, {
        method: "PUT", body: "retained", headers: { "Content-Length": "8" },
      })).status).toBe(403);
      expect((await request(binding, { method: "DELETE" })).status).toBe(403);
    }
    expect((await request("NANOCODEX_WORKSPACES", { method: "DELETE" })).status).toBe(204);
  });

  it("keeps the restored handler behind existing R2 binding and copy guards", async () => {
    const handler = Sandbox.outboundHandlers!.r2EgressMount!;
    const context = { containerId: "retained-sandbox", className: "Sandbox", params: configuration().outboundByHostOverrides["r2.internal"].params };
    for (const request of [
      new Request("http://r2.internal/UNMOUNTED/file"),
      new Request("http://r2.internal/NANOCODEX_WORKSPACES/file", {
        method: "PUT", headers: { "x-amz-copy-source": "/NANOCODEX_WORKSPACES_0/file" },
      }),
      new Request("http://r2.internal/NANOCODEX_BRAIN/file"),
      new Request("https://example.com/file"),
    ]) {
      expect((await handler(request, env, context)).status).toBe(403);
    }
    expect((await handler(new Request("http://r2.internal/NANOCODEX_WORKSPACES/file"), env, {
      ...context, params: undefined,
    })).status).toBe(403);
  });
});
