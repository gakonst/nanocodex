import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DurableEventLog } from "../src/durable-events";
import { ManagedEventArchive } from "../src/managed-event-archive";

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

for (const archived of [false, true]) {
  it(`streams through ${archived ? "archive boundaries" : "byte-limited pages"} without another publication`, async () => {
    const runtime = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace; NANOCODEX_HISTORY: R2Bucket };
    await runInDurableObject(runtime.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (_instance, ctx) => {
      const log = new DurableEventLog<{ type: string; text: string }>(ctx.storage);
      const archive = new ManagedEventArchive<{ type: string; text: string }>(ctx.storage,
        runtime.NANOCODEX_HISTORY, ctx.id.toString(),
        { segmentTargetBytes: 160, sealThresholdBytes: 1, recentEventCount: 1 });
      const abort = new AbortController();
      const timeout = setTimeout(() => abort.abort(), 2_000);
      let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
      try {
        for (let index = 0; index < 7; index++) {
          log.append({ type: "event", text: "x".repeat(archived ? 200 : 1_100_000) });
        }
        if (archived) while ((await archive.seal(true)).sealed) { /* retain live tail */ }
        const page = archived ? archive.pageReader(log) : async (after: string, limit: number) => log.page(after, limit);
        const first = await page("0", 256);
        expect(first.length).toBeGreaterThan(0);
        expect(first.length).toBeLessThan(7);
        const cursors: string[] = [];
        reader = log.streamWithPage("0", archive.latestCursor(log), page, abort.signal).body!.getReader();
        while (cursors.length < 7) {
          const chunk = await reader.read();
          if (chunk.done) break;
          for (const match of new TextDecoder().decode(chunk.value).matchAll(/^id: (\d+)$/gm)) cursors.push(match[1]!);
        }
        expect(cursors).toEqual(["1", "2", "3", "4", "5", "6", "7"]);
      } finally {
        clearTimeout(timeout);
        await reader?.cancel();
        await archive.deleteAll();
        log.clear();
      }
    });
  });
}

it("does not fetch the next short page before delivery or after cancellation", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string }>(ctx.storage);
    log.append({ type: "event" });
    log.append({ type: "event" });
    const requested: string[] = [];
    const reader = log.streamWithPage("0", "2", async (after) => {
      requested.push(after);
      return log.page(after, 1);
    }).body!.getReader();
    try {
      await reader.read(); // Initial cursor comment; first event remains backpressured.
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(requested).toEqual(["0"]);
      await reader.cancel();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(requested).toEqual(["0"]);
    } finally { await reader.cancel(); log.clear(); }
  });
});
