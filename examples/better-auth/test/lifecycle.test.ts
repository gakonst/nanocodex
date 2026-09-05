import assert from "node:assert/strict";
import { test } from "node:test";

import { createConnectLifecycle, revokeHostPrincipal } from "../src/lifecycle.ts";

test("logout aborts and awaits Connect before revoke, Connect logout, and UI clearing", async () => {
  const lifecycle = createConnectLifecycle();
  const events: string[] = [];
  let release: (() => void) | undefined;
  let started!: () => void;
  const running = new Promise<void>((resolve) => { started = resolve; });
  const connect = lifecycle.run(async (signal) => {
    events.push("connect.start");
    signal.addEventListener("abort", () => events.push("connect.abort"), { once: true });
    started();
    await new Promise<void>((resolve) => { release = resolve; });
    events.push("connect.settle");
  });
  await running;

  const cleanup = lifecycle.beforeProviderLogout({
    async revoke() { events.push("revoke"); },
    async logoutConnect() { events.push("connect.logout"); },
    clearUi() { events.push("ui.clear"); },
  });
  assert.deepEqual(events, ["connect.start", "connect.abort"]);
  release?.();
  await Promise.all([connect, cleanup]);
  assert.deepEqual(events, [
    "connect.start",
    "connect.abort",
    "connect.settle",
    "revoke",
    "connect.logout",
    "ui.clear",
  ]);
});

test("failed revocation preserves the provider and Connect sessions for retry", async () => {
  const events: string[] = [];
  await assert.rejects(createConnectLifecycle().beforeProviderLogout({
    async revoke() { events.push("revoke"); throw new Error("offline"); },
    async logoutConnect() { events.push("connect.logout"); },
    clearUi() { events.push("ui.clear"); },
  }), /offline/);
  assert.deepEqual(events, ["revoke"]);
});

test("host revocation treats every non-2xx response as a fence failure", async () => {
  let method: string | undefined;
  await assert.rejects(revokeHostPrincipal(async (_input, init) => {
    method = init?.method;
    return Response.json({ error: "unauthorized" }, { status: 401 });
  }), /failed \(401\)/);
  assert.equal(method, "DELETE");
  await assert.doesNotReject(revokeHostPrincipal(async () => new Response(null, { status: 204 })));
});
