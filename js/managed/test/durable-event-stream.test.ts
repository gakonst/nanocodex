import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import { DurableEventLog } from "../src/durable-events";

it("checks durable progress before heartbeats and closes a stale owner's stream", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string }>(ctx.storage);
    let heartbeat: () => void = () => { throw new Error("heartbeat not installed"); };
    const interval = vi.spyOn(globalThis, "setInterval").mockImplementation((callback) => {
      heartbeat = callback as () => void;
      return 0 as unknown as ReturnType<typeof setInterval>;
    });
    let stale = false;
    const page = async (after: string, limit: number) => {
      if (stale) throw new Error("Durable Object instance is no longer active");
      return log.page(after, limit);
    };
    const reader = log.streamWithPage("0", log.latestCursor(), page).body!.getReader();
    try {
      await reader.read(); // Initial retry/cursor frame.
      await new Promise((resolve) => setTimeout(resolve, 0));
      log.append({ type: "missed_publication" });
      heartbeat();
      expect(new TextDecoder().decode((await reader.read()).value)).toContain("id: 1\n");
      expect(new TextDecoder().decode((await reader.read()).value)).toContain(": keepalive");
      await new Promise((resolve) => setTimeout(resolve, 0));
      stale = true;
      heartbeat();
      expect((await reader.read()).done).toBe(true);
    } finally {
      await reader.cancel();
      interval.mockRestore();
      log.clear();
    }
  });
});

it("releases event stream slots when readers disconnect repeatedly", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string }>(ctx.storage);
    try {
      for (let index = 0; index < 40; index++) {
        const response = log.stream("0");
        expect(response.status, `reconnect ${index}`).toBe(200);
        const reader = response.body!.getReader();
        expect((await reader.read()).done).toBe(false);
        await reader.cancel();
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
    } finally {
      log.clear();
    }
  });
});

it("releases event stream slots across Durable Object fetch disconnects", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  const stub = namespace.getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (instance, ctx) => {
    const log = new DurableEventLog<{ type: string }>(ctx.storage);
    Object.defineProperty(instance, "fetch", { value: (request: Request) => {
      if (new URL(request.url).pathname === "/clear") {
        log.clear();
        return new Response("cleared");
      }
      return log.stream("0", request.signal);
    } });
  });
  try {
    for (let index = 0; index < 40; index++) {
      const response = await stub.fetch("https://memory.internal/events");
      expect(response.status, `reconnect ${index}`).toBe(200);
      const reader = response.body!.getReader();
      if (index % 2 === 0) expect((await reader.read()).done).toBe(false);
      await reader.cancel();
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  } finally {
    await stub.fetch("https://memory.internal/clear");
  }
});

it("pages large chunked payloads by bytes without losing cursors", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string; text: string }>(ctx.storage);
    for (let index = 0; index < 7; index++) log.append({ type: "large", text: "x".repeat(1_100_000) });
    const newest = log.history(undefined, 256);
    expect(newest.data.map((event) => event.cursor)).toEqual(["5", "6", "7"]);
    expect(newest.has_more).toBe(true);
    const middle = log.history("5", 256);
    expect(middle.data.map((event) => event.cursor)).toEqual(["2", "3", "4"]);
    expect(log.history("2", 256).has_more).toBe(false);
    expect(log.page("0").map((event) => event.cursor)).toEqual(["1", "2", "3"]);
    expect(log.page("3").map((event) => event.cursor)).toEqual(["4", "5", "6"]);
    // A single oversized event still makes progress, rather than hiding a turn.
    log.append({ type: "oversized", text: "y".repeat(4_300_000) });
    expect(log.history(undefined, 256).data.map((event) => event.cursor)).toEqual(["8"]);
    log.clear();
  });
});
