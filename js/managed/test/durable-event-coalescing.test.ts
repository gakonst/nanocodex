import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DurableEventLog } from "../src/durable-events";

type Message = { type: string; text?: string; seq?: number };
const merge = (staged: Message, next: Message): Message | undefined =>
  staged.text === undefined || next.text === undefined ? undefined : { ...staged, text: staged.text + next.text, seq: next.seq };

function withLog(run: (log: DurableEventLog<Message>, storage: DurableObjectStorage) => void | Promise<void>) {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  return runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const log = new DurableEventLog<Message>(ctx.storage);
    try { await run(log, ctx.storage); } finally { log.clear(); }
  });
}

it("merges consecutive same-key deltas into one row written before the next event", () => withLog((log) => {
  for (let seq = 1; seq <= 12; seq++) expect(log.stage({ type: "delta", text: `w${seq} `, seq }, "turn", "k", merge, 3, 1_000)).toBeUndefined();
  expect(log.latestCursor()).toBe("0");
  const message = log.append({ type: "message", text: "final" }, "turn");
  const rows = log.page("0", 10);
  expect(rows.map(row => row.message)).toEqual([
    { type: "delta", text: Array.from({ length: 12 }, (_, i) => `w${i + 1} `).join(""), seq: 12 },
    { type: "message", text: "final" },
  ]);
  expect(rows.map(row => row.cursor)).toEqual(["1", "2"]);
  // The materialized row is broadcast exactly once, immediately before its follower.
  expect(log.takeMaterialized(message).map(row => row.cursor)).toEqual(["1"]);
  expect(log.takeMaterialized(message)).toEqual([]);
}));

it("flushes on key change, size bound and explicit flush, preserving arrival order", () => withLog((log) => {
  expect(log.stage({ type: "delta", text: "a", seq: 1 }, "turn", "item-1", merge, 1, 2)).toBeUndefined();
  expect(log.stage({ type: "delta", text: "b", seq: 2 }, "turn", "item-1", merge, 1, 2)).toBeUndefined();
  const bounded = log.stage({ type: "delta", text: "c", seq: 3 }, "turn", "item-1", merge, 1, 2);
  expect(bounded?.message).toEqual({ type: "delta", text: "ab", seq: 2 });
  const switched = log.stage({ type: "delta", text: "x", seq: 4 }, "turn", "item-2", merge, 1, 2);
  expect(switched?.message).toEqual({ type: "delta", text: "c", seq: 3 });
  const other = log.stage({ type: "delta", text: "y", seq: 5 }, "other-turn", "item-2", merge, 1, 2);
  expect(other?.message).toEqual({ type: "delta", text: "x", seq: 4 });
  expect(log.flushStaged()?.message).toEqual({ type: "delta", text: "y", seq: 5 });
  expect(log.flushStaged()).toBeUndefined();
  expect(log.page("0", 10).map(row => [row.cursor, row.turn_id, row.message.text])).toEqual([
    ["1", "turn", "ab"], ["2", "turn", "c"], ["3", "turn", "x"], ["4", "other-turn", "y"],
  ]);
}));

it("never broadcasts a materialized row whose transaction rolled back", () => withLog((log, storage) => {
  log.stage({ type: "delta", text: "lost", seq: 1 }, "turn", "k", merge, 4, 1_000);
  expect(() => storage.transactionSync(() => {
    log.append({ type: "message" }, "turn");
    throw new Error("abort");
  })).toThrow("abort");
  expect(log.latestCursor()).toBe("0");
  const replacement = log.append({ type: "replacement" }, "turn");
  expect(replacement.cursor).toBe("1");
  expect(log.takeMaterialized(replacement)).toEqual([]);
  expect(log.page("0", 10).map(row => row.message.type)).toEqual(["replacement"]);
}));

it("discards staged deltas on clear", () => withLog((log) => {
  log.stage({ type: "delta", text: "gone", seq: 1 }, "turn", "k", merge, 4, 1_000);
  log.clear();
  expect(log.hasStaged()).toBe(false);
  expect(log.flushStaged()).toBeUndefined();
  expect(log.latestCursor()).toBe("0");
}));
