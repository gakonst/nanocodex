import assert from "node:assert/strict";
import test from "node:test";
import { IDBFactory, IDBKeyRange } from "fake-indexeddb";
import type { AgentEvent } from "nanocodex-react/agent";
import { createLocalTranscriptJournal, type LocalTranscriptTurn } from "./localTranscriptJournal.ts";
import { localContextTurns, localTerminalAgent, localTranscriptEvents } from "./localAgentRuntime.ts";

test("local context bootstrap and projection do not discard messages or steering details", () => {
  const history = Array.from({ length: 300 }, (_, index) => [
    { type: "message", role: "user", content: [{ type: "input_text", text: `prompt-${index}` }] },
    { type: "message", role: "assistant", content: [{ type: "output_text", text: `answer-${index}` }] },
  ]).flat();
  const turns = localContextTurns(history, "thread");
  assert.equal(turns.length, 300);
  assert.equal(localTranscriptEvents(turns, "session").length, 600);
  const pending = Array.from({ length: 100 }, (_, index): LocalTranscriptTurn => ({
    threadId: "thread", turnId: `pending-${index}`, createdAt: index, prompt: "pending", status: "blocked",
    steers: Array.from({ length: 40 }, (_, steer) => ({ id: `steer-${steer}`, text: `detail-${steer}`, status: "accepted" })),
  }));
  const projected = localTranscriptEvents(pending, "session");
  assert.equal(projected.filter(({ type }) => type === "managed.prompt").length, 100);
  assert.equal(projected.filter(({ type }) => type === "managed.steer").length, 4000);
  assert.equal(projected.filter(({ type }) => type === "run.failed").length, 100);
  assert.ok(projected.every(({ payload }) => !payload.detail_truncated));
});

test("the watcher contract pages all local history and catches up after a long inactive gap", async (t) => {
  const journal = createLocalTranscriptJournal({ indexedDB: new IDBFactory(), keyRange: IDBKeyRange, databaseName: crypto.randomUUID(), broadcastChannel: null });
  const entries = Array.from({ length: 300 }, (_, index): LocalTranscriptTurn => ({
    threadId: "thread", turnId: `turn-${index}`, createdAt: index, prompt: `prompt-${index}`, assistant: `answer-${index}`, status: "completed",
  }));
  await journal.bootstrap("thread", entries);
  let refresh = () => {};
  const agent = {
    sessionId: "session", session: { context: async () => { throw new Error("initialized journal must not reload model context"); } },
    turn: { prompt: () => { throw new Error("completed history must not execute"); } },
    events: { watch: () => ({ onEvent: () => () => {}, off() {} }) },
  } as unknown as Parameters<typeof localTerminalAgent>[0];
  const terminal = localTerminalAgent(agent, "thread", journal, (error) => { throw error; }, 30_000, {
    watch(listener) { refresh = listener; return () => {}; },
  });
  const watcher = terminal.events.watch(); t.after(() => watcher.off());
  let latest: readonly AgentEvent[] = [];
  let historyChanged = () => {};
  await new Promise<void>((resolve) => { watcher.onHistory!((events) => { latest = events; resolve(); historyChanged(); }); });
  assert.equal(latest.filter(({ type }) => type === "managed.prompt").length, 128);
  while (await watcher.loadOlder!()) { /* all history remains reachable */ }
  assert.equal(latest.filter(({ type }) => type === "managed.prompt").length, 300);
  for (let index = 300; index < 570; index++) {
    const entry = { threadId: "thread", turnId: `turn-${index}`, createdAt: index, prompt: `prompt-${index}` };
    await journal.recordPrompt(entry); await journal.completeTurn({ ...entry, assistant: `answer-${index}` });
  }
  await new Promise<void>((resolve) => { historyChanged = resolve; refresh(); });
  assert.equal(latest.filter(({ type }) => type === "managed.prompt").length, 570);
  assert.deepEqual(latest.filter(({ type }) => type === "managed.prompt").map(({ payload }) => payload.text),
    Array.from({ length: 570 }, (_, index) => `prompt-${index}`));
});

test("a competing tab's terminal winner is absorbed if it leaves the pending index before recovery reads it", async () => {
  const retained = createLocalTranscriptJournal({ indexedDB: new IDBFactory(), keyRange: IDBKeyRange, databaseName: crypto.randomUUID(), broadcastChannel: null });
  await retained.bootstrap("thread", []);
  const journal = { ...retained, async *pending(threadId: string) {
    const page = await retained.load(threadId);
    for (const turn of page.turns) await retained.completeTurn({ ...turn, assistant: "other tab completed this" });
    yield* retained.pending(threadId);
  } };
  const agent = {
    sessionId: "session", session: { context: async () => { throw new Error("already initialized"); } },
    turn: { prompt: () => { throw new Error("must not replay the competing winner"); } },
    events: { watch: () => ({ onEvent: () => () => {}, off() {} }) },
  } as unknown as Parameters<typeof localTerminalAgent>[0];
  const terminal = localTerminalAgent(agent, "thread", journal);
  const result = await terminal.turn.prompt({ input: "race" }).result();
  assert.equal(result.finalMessage, "other tab completed this");
  result.dispose();
});

test("recovery drains more than one pending cursor page even when unfinished turns precede old completed history", { timeout: 10_000 }, async (t) => {
  const journal = createLocalTranscriptJournal({ indexedDB: new IDBFactory(), keyRange: IDBKeyRange, databaseName: crypto.randomUUID(), broadcastChannel: null });
  const entries = Array.from({ length: 450 }, (_, index): LocalTranscriptTurn => ({
    threadId: "thread", turnId: `turn-${index}`, createdAt: index, prompt: `prompt-${index}`,
    ...(index < 150 ? { status: "pending" } : { status: "completed", assistant: "old completed answer" }),
  }));
  await journal.bootstrap("thread", entries);
  const dispatched: string[] = [];
  let finished = () => {};
  const completion = new Promise<void>((resolve) => { finished = resolve; });
  const agent = {
    sessionId: "session", session: { context: async () => { throw new Error("already initialized"); } },
    turn: { prompt: ({ id }: { id: string }) => {
      dispatched.push(id);
      return { result: async () => ({ finalMessage: `recovered ${id}`, dispose() {} }),
        dispose() { if (id === "turn-149") finished(); } };
    } },
    events: { watch: () => ({ onEvent: () => () => {}, off() {} }) },
  } as unknown as Parameters<typeof localTerminalAgent>[0];
  const terminal = localTerminalAgent(agent, "thread", journal);
  const watcher = terminal.events.watch(); t.after(() => watcher.off());
  watcher.onHistory!(() => {});
  await completion;
  assert.deepEqual(dispatched, Array.from({ length: 150 }, (_, index) => `turn-${index}`));
  assert.equal((await journal.get("thread", "turn-0"))?.assistant, "recovered turn-0");
  assert.equal((await journal.get("thread", "turn-149"))?.assistant, "recovered turn-149");
  const pending = [];
  for await (const entry of journal.pending("thread")) pending.push(entry);
  assert.deepEqual(pending, []);
});

test("tab activation coalesces refresh bursts and reuses unchanged projected history", async (t) => {
  const retained = createLocalTranscriptJournal({ indexedDB: new IDBFactory(), keyRange: IDBKeyRange, databaseName: crypto.randomUUID(), broadcastChannel: null });
  await retained.bootstrap("thread", [{ threadId: "thread", turnId: "old", createdAt: 1, prompt: "old prompt", assistant: "old answer", status: "completed" }]);
  let reads = 0;
  const journal = { ...retained, async load(threadId: string, before?: string) { reads++; return retained.load(threadId, before); } };
  let refresh = () => {};
  const agent = {
    sessionId: "session", session: { context: async () => { throw new Error("already initialized"); } },
    turn: { prompt: () => { throw new Error("must not execute"); } },
    events: { watch: () => ({ onEvent: () => () => {}, off() {} }) },
  } as unknown as Parameters<typeof localTerminalAgent>[0];
  const terminal = localTerminalAgent(agent, "thread", journal, error => { throw error; }, 30_000, {
    watch(listener) { refresh = listener; return () => {}; },
  });
  const watcher = terminal.events.watch(); t.after(() => watcher.off());
  let latest: readonly AgentEvent[] = [];
  let publications = 0;
  await new Promise<void>(resolve => watcher.onHistory!(events => { latest = events; publications++; resolve(); }));
  await new Promise(resolve => setTimeout(resolve, 20));
  const initial = latest;
  const before = reads;
  refresh(); refresh(); refresh();
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(reads - before, 1);
  assert.equal(publications, 1);
  assert.equal(latest, initial);
  const entry = { threadId: "thread", turnId: "new", createdAt: 2, prompt: "new prompt" };
  await retained.recordPrompt(entry);
  await retained.completeTurn({ ...entry, assistant: "new answer" });
  refresh();
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(latest.filter(event => event.type === "managed.prompt").length, 2);
  assert.equal(latest.filter(event => event.type === "assistant.message").at(-1)?.payload.text, "new answer");
  assert.equal(initial.filter(event => event.type === "managed.prompt").length, 1);
});
