import assert from "node:assert/strict";
import { test } from "node:test";
import { clearSessionBeforeProviderChange, createSessionFence } from "../src/session-fence.ts";

test("revocation and agent shutdown complete before Connect and provider logout", async () => {
  const events: string[] = [];
  await clearSessionBeforeProviderChange({
    async revoke() { events.push("revoke"); },
    async shutdownAgent() { events.push("agent.shutdown"); },
    async logoutConnect() { events.push("connect.logout"); },
    clearUi() { events.push("ui.clear"); },
  });
  events.push("privy.logout");
  assert.deepEqual(events, [
    "revoke",
    "agent.shutdown",
    "connect.logout",
    "ui.clear",
    "privy.logout",
  ]);
});

test("failed revocation preserves Connect state and blocks provider logout", async () => {
  const events: string[] = [];
  await assert.rejects(clearSessionBeforeProviderChange({
    async revoke() {
      events.push("revoke");
      throw new Error("upstream unavailable");
    },
    async logoutConnect() { events.push("connect.logout"); },
    clearUi() { events.push("ui.clear"); },
  }), /upstream unavailable/);
  assert.deepEqual(events, ["revoke"]);
});

test("logout aborts and awaits a late connection before revocation", async () => {
  const fence = createSessionFence();
  const events: string[] = [];
  let release: (() => void) | undefined;
  let started: (() => void) | undefined;
  const didStart = new Promise<void>((resolve) => { started = resolve; });
  const connection = fence.run(async (signal) => {
    events.push("connect.start");
    signal.addEventListener("abort", () => events.push("connect.abort"), { once: true });
    started?.();
    await new Promise<void>((resolve) => { release = resolve; });
    events.push("connect.settled");
  });
  await didStart;

  const cleanup = fence.beforeProviderChange({
    async revoke() { events.push("revoke"); },
    async shutdownAgent() { events.push("agent.shutdown"); },
    async logoutConnect() { events.push("connect.logout"); },
    clearUi() { events.push("ui.clear"); },
  });
  assert.deepEqual(events, ["connect.start", "connect.abort"]);
  release?.();
  await Promise.all([connection, cleanup]);
  assert.deepEqual(events, [
    "connect.start",
    "connect.abort",
    "connect.settled",
    "revoke",
    "agent.shutdown",
    "connect.logout",
    "ui.clear",
  ]);
});

test("a replacement can cancel and await a stale reconnect", async () => {
  const fence = createSessionFence();
  const events: string[] = [];
  let release: (() => void) | undefined;
  let markStarted: (() => void) | undefined;
  const started = new Promise<void>((resolve) => { markStarted = resolve; });
  const stale = fence.run(async (signal) => {
    signal.addEventListener("abort", () => events.push("stale.abort"), { once: true });
    markStarted?.();
    await new Promise<void>((resolve) => { release = resolve; });
    events.push("stale.done");
  });
  await started;
  const cancelled = fence.cancel();
  release?.();
  await Promise.all([stale, cancelled]);
  await fence.run(async () => { events.push("replacement.run"); });
  assert.deepEqual(events, ["stale.abort", "stale.done", "replacement.run"]);
});
