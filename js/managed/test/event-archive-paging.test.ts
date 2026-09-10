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
