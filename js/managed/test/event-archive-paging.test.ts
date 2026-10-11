import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DurableEventLog } from "../src/durable-events";
import { ManagedEventArchive } from "../src/managed-event-archive";

it("walks local and archived pages completely while retaining one archive segment per page", async () => {
  const runtime = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace; NANOCODEX_HISTORY: R2Bucket };
  await runInDurableObject(runtime.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string; text: string }>(ctx.storage);
    const archive = new ManagedEventArchive<{ type: string; text: string }>(ctx.storage, runtime.NANOCODEX_HISTORY,
      ctx.id.toString(), { segmentTargetBytes: 160, sealThresholdBytes: 1, recentEventCount: 1 });
    for (let i = 0; i < 36; i++) log.append({ type: "message", text: String(i).repeat(100) });
    try {
      while ((await archive.seal(true)).sealed) { /* archive all but the live tail */ }
      const forward: string[] = [];
      let after = "0";
      while (true) {
        const page = await archive.page(log, after, 256);
        if (page.length === 0) break;
        expect(page.length).toBeLessThanOrEqual(2);
        forward.push(...page.map((event) => event.cursor)); after = page.at(-1)!.cursor;
      }
      expect(forward).toEqual(Array.from({ length: 36 }, (_, i) => String(i + 1)));
      let before: string | undefined;
      const backward: string[] = [];
      while (true) {
        const page = await archive.history(log, before, 256);
        expect(page.data.length).toBeGreaterThan(0);
        expect(page.data.length).toBeLessThanOrEqual(2);
        backward.unshift(...page.data.map((event) => event.cursor));
        if (!page.has_more) break;
        before = page.data[0]!.cursor;
      }
      expect(backward).toEqual(forward);
    } finally { await archive.deleteAll(); log.clear(); }
  });
});


it("reuses verified immutable segments across cold archive readers without reading R2 again", async () => {
  const runtime = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace; NANOCODEX_HISTORY: R2Bucket };
  await runInDurableObject(runtime.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string; text: string }>(ctx.storage);
    const archive = new ManagedEventArchive(ctx.storage, runtime.NANOCODEX_HISTORY, ctx.id.toString());
    log.append({ type: "message", text: "retained private history" });
    log.append({ type: "message", text: "live tail" });
    try {
      await archive.seal(true);
      let reads = 0;
      const bucket = { get: (key: string) => { reads++; return runtime.NANOCODEX_HISTORY.get(key); } } as R2Bucket;
      const reader = () => new ManagedEventArchive<{ type: string; text: string }>(ctx.storage, bucket, ctx.id.toString());
      const first = await reader().history(log, "2", 128);
      expect(reads).toBe(1);
      const second = await reader().history(log, "2", 128);
      expect(second).toEqual(first);
      expect(reads).toBe(1);
      expect(second.data[0]!.message.text).toBe("retained private history");
      // A damaged cache entry is disposable; the durable object repairs it
      // from R2 instead of allowing cached bytes into a transcript.
      const state = archive.portableState();
      const descriptor = JSON.parse(state.recent_json)[0];
      const cache = await caches.open("nanocodex-managed-event-segments-v1");
      await cache.put(`https://managed-history.internal/${descriptor.key}`, new Response("broken", {
        headers: { "cache-control": "public, max-age=86400" },
      }));
      expect(await reader().history(log, "2", 128)).toEqual(first);
      expect(reads).toBe(2);
    } finally { await archive.deleteAll(); log.clear(); }
  });
});


it("concurrent readers at different archived segments decode one segment at a time and retain at most one", async () => {
  const runtime = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace; NANOCODEX_HISTORY: R2Bucket };
  await runInDurableObject(runtime.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string; text: string }>(ctx.storage);
    const policy = { segmentTargetBytes: 160, sealThresholdBytes: 1, recentEventCount: 1 };
    const writer = new ManagedEventArchive<{ type: string; text: string }>(ctx.storage, runtime.NANOCODEX_HISTORY,
      ctx.id.toString(), policy);
    for (let i = 0; i < 36; i++) log.append({ type: "message", text: String(i % 10).repeat(100) });
    let inFlight = 0, maxInFlight = 0, decodes = 0;
    // Each body read stays in flight briefly so overlapping decodes are observable.
    const bucket = { get: async (key: string) => {
      const object = await runtime.NANOCODEX_HISTORY.get(key);
      if (!object || !key.includes("/segments/")) return object;
      return new Proxy(object, { get(target, property) {
        if (property === "arrayBuffer") return async () => {
          inFlight++; decodes++; maxInFlight = Math.max(maxInFlight, inFlight);
          try { await new Promise((resolve) => setTimeout(resolve, 10)); return await target.arrayBuffer(); }
          finally { inFlight--; }
        };
        const value = Reflect.get(target, property, target);
        return typeof value === "function" ? value.bind(target) : value;
      } });
    } } as unknown as R2Bucket;
    try {
      while ((await writer.seal(true)).sealed) { /* archive all but the live tail */ }
      const archive = new ManagedEventArchive<{ type: string; text: string }>(ctx.storage, bucket, ctx.id.toString(), policy);
      const starts = ["0", "6", "12", "18", "24", "30"];
      const readers = starts.map(() => archive.pageReader(log));
      // Like many SSE subscribers resuming from different cursors after a wake.
      const firstPages = await Promise.all(readers.map((read, index) => read(starts[index]!, 256)));
      expect(firstPages.map((page) => page[0]?.cursor)).toEqual(starts.map((start) => String(Number(start) + 1)));
      expect(maxInFlight).toBe(1);
      expect(decodes).toBe(starts.length);
      expect(archive.residentSegments()).toBeLessThanOrEqual(1);
      for (const [index, read] of readers.entries()) {
        let after = firstPages[index]!.at(-1)!.cursor;
        const seen = firstPages[index]!.map((event) => event.cursor);
        while (true) {
          const page = await read(after, 256);
          expect(archive.residentSegments()).toBeLessThanOrEqual(1);
          if (page.length === 0) break;
          seen.push(...page.map((event) => event.cursor));
          after = page.at(-1)!.cursor;
        }
        expect(seen).toEqual(Array.from({ length: 36 - Number(starts[index]) }, (_, offset) => String(Number(starts[index]) + offset + 1)));
        // A caught-up reader releases the decoded segment it last used.
        expect(archive.residentSegments()).toBe(0);
      }
      expect(maxInFlight).toBe(1);
    } finally { await writer.deleteAll(); log.clear(); }
  });
});

it("seals large and chunked events byte-for-byte while reading bounded source batches", async () => {
  const runtime = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace; NANOCODEX_HISTORY: R2Bucket };
  await runInDurableObject(runtime.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<{ type: string; text: string }>(ctx.storage);
    // Twelve 300 KB direct rows and a 1.5 MB chunked row with astral characters.
    for (let i = 0; i < 12; i++) log.append({ type: "message", text: String(i % 10).repeat(300_000) });
    log.append({ type: "message", text: "Ελληνικά 😀 ".repeat(110_000) });
    log.append({ type: "message", text: "live tail" });
    let maxRowChars = 0;
    const measured = {
      sql: { exec: (query: string, ...args: unknown[]) => {
        const rows = ctx.storage.sql.exec(query, ...(args as SqlStorageValue[])).toArray();
        let chars = 0;
        for (const row of rows) if (typeof row.message_json === "string") chars += row.message_json.length;
        maxRowChars = Math.max(maxRowChars, chars);
        return { toArray: () => rows, one: () => rows[0], [Symbol.iterator]: () => rows[Symbol.iterator]() };
      } },
      transactionSync: <T>(callback: () => T) => ctx.storage.transactionSync(callback),
    } as unknown as DurableObjectStorage;
    const source = log.page("0", 256, { maxBytes: 64 * 1024 * 1024 });
    const archive = new ManagedEventArchive<{ type: string; text: string }>(measured, runtime.NANOCODEX_HISTORY, ctx.id.toString());
    try {
      const sealed = await archive.seal(true);
      expect(sealed.sealed).toBe(true);
      expect(sealed.archived_events).toBe(13);
      // The selected segment holds about 5.2 MB of source rows. No single read
      // may materialize more than one bounded batch (or one chunk) of them.
      expect(maxRowChars).toBeLessThanOrEqual(1024 * 1024);
      // Segments keep their exact historical encoding, so a seal retried after
      // a crash reproduces the same content-addressed objects.
      const expected = '{"version":1,"kind":"managed_event_segment","events":['
        + source.slice(0, 13).map((event) => '{"cursor":' + JSON.stringify(event.cursor) + ',"created_at":' + event.created_at
          + ',"message":' + JSON.stringify(event.message) + ',"turn_id":' + JSON.stringify(event.turn_id) + '}').join(",") + ']}';
      const digest = [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(expected)))]
        .map((byte) => byte.toString(16).padStart(2, "0")).join("");
      expect(sealed.segment_key).toMatch(new RegExp("-" + digest + "\\.json$"));
      const replayed = await new ManagedEventArchive<{ type: string; text: string }>(ctx.storage, runtime.NANOCODEX_HISTORY, ctx.id.toString())
        .page(log, "0", 256);
      expect(replayed.map((event) => event.message)).toEqual(source.slice(0, 13).map((event) => event.message));
    } finally { await archive.deleteAll(); log.clear(); }
  });
});

