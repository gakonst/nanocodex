import { createExecutionContext, env } from "cloudflare:test";
import { expect, it } from "vitest";
import worker, { type AccountHostedTools } from "../src/index";
import type { Principal } from "../src/account-auth";

it("lists only the owner's live Hands without private routing metadata", async () => {
  const owner = crypto.randomUUID(), other = crypto.randomUUID();
  const principal: Principal = {
    kind: "api_key", userId: owner,
    organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    role: "owner", subjectId: `user:${owner}`, credentialId: "test", authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"],
  };
  const call = (actor = principal, method = "GET", suffix = "") => worker.fetch(
    new Request("https://nanocodex.example/v1/account/hands" + suffix, { method }),
    env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
  );
  expect(await (await call()).json()).toEqual({ data: [] });
  const namespace = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace<AccountHostedTools> }).NANOCODEX_ACCOUNT_TOOLS;
  const upgraded = await namespace.getByName(owner).fetch("https://account-tools.internal/tool-host", {
    headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner },
  });
  const socket = upgraded.webSocket!; socket.accept();
  try {
    const ready = new Promise((resolve, reject) => {
      socket.addEventListener("message", event => resolve(JSON.parse(String(event.data))), { once: true });
      socket.addEventListener("close", event => reject(new Error(`Hand closed: ${event.code} ${event.reason}`)), { once: true });
      socket.addEventListener("error", () => reject(new Error("Hand socket failed")), { once: true });
    });
    socket.send(JSON.stringify({ type: "catalog", attachment_id: "ios-phone", machines: [{
      id: "ios-phone", name: "iPhone", workspace: "/private/device/workspace", capabilities: ["native", "background_limited"],
    }], tools: [{ provider: "native", remote_name: "device_info", parallel_safe: true, timeout_ms: 15000,
      definition: { type: "function", name: "device_info", description: "Device info", strict: false,
        parameters: { type: "object", properties: {}, required: [], additionalProperties: false } },
    }] }));
    expect(await ready).toEqual({ type: "ready" });
    const response = await call();
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(await response.json()).toEqual({ data: [{
      id: "ios-phone", name: "iPhone", workspace: "/ios-phone", capabilities: ["native", "background_limited"],
    }] });
    expect(await (await call({ ...principal, userId: other })).json()).toEqual({ data: [] });
    expect((await call({ ...principal, capabilities: ["agents:read"] })).status).toBe(403);
    expect((await call({ ...principal, connectGrant: { grantId: "test" } as NonNullable<Principal["connectGrant"]> })).status).toBe(403);
    expect((await call(principal, "POST")).status).toBe(405);
    expect((await call(principal, "GET", "?owner=" + other)).status).toBe(400);
  } finally { socket.close(1000, "Done"); }
  await expect.poll(async () => (await (await call()).json() as { data: unknown[] }).data.length).toBe(0);
});
