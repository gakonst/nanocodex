import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DurableEventLog } from "../src/durable-events";

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
