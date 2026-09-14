import assert from "node:assert/strict";
import test from "node:test";
import { IDBFactory, IDBKeyRange } from "fake-indexeddb";
import { createLocalTranscriptJournal, type LocalTranscriptJournal, type LocalTranscriptTurn } from "./localTranscriptJournal.ts";

function fixture() {
  const indexedDB = new IDBFactory();
  const databaseName = crypto.randomUUID();
  const options = { indexedDB, keyRange: IDBKeyRange, databaseName, broadcastChannel: null };
  return { indexedDB, databaseName, journal: createLocalTranscriptJournal(options), reopen: () => createLocalTranscriptJournal(options) };
}
const turn = (index: number, status: LocalTranscriptTurn["status"] = "completed"): LocalTranscriptTurn => ({
  threadId: "thread", turnId: `turn-${index}`, createdAt: index, prompt: `prompt-${index}`, status,
  ...(status === "completed" ? { assistant: `answer-${index}` } : {}),
});
async function allTurns(journal: LocalTranscriptJournal) {
  const turns: LocalTranscriptTurn[] = [];
  let before: string | undefined;
  let pages = 0;
  do {
    const page = await journal.load("thread", before);
    assert.ok(page.turns.length <= 128, "one read stays paged without truncating storage");
    turns.unshift(...page.turns); before = page.next; pages++;
  } while (before !== undefined);
  return { turns, pages };
}

test("bootstrap and later terminal transitions retain all durable history across reopen and cursor pages", async () => {
  const { journal, reopen } = fixture();
  await journal.bootstrap("thread", Array.from({ length: 300 }, (_, index) => turn(index)));
  for (let index = 300; index < 450; index++) {
    await journal.recordPrompt(turn(index, "pending"));
    if (index % 2) await journal.completeTurn(turn(index));
    else await journal.updateTurn(turn(index), { status: "cancelled" });
  }
  const retained = await allTurns(reopen());
  assert.equal(retained.pages, 4);
  assert.deepEqual(retained.turns.map(({ turnId }) => turnId), Array.from({ length: 450 }, (_, index) => `turn-${index}`));
  assert.equal((await journal.get("thread", "turn-0"))?.assistant, "answer-0");
  assert.equal((await journal.load("different-thread")).turns.length, 0);
});

test("more than 32 pending turns and steers survive recovery without scanning completed history", async () => {
  const { journal, reopen } = fixture();
  await journal.bootstrap("thread", []);
  await journal.recordPrompt(turn(0, "pending"));
  for (let index = 1; index <= 300; index++) {
    await journal.recordPrompt(turn(index, "pending"));
    await journal.completeTurn(turn(index));
  }
  for (let index = 301; index <= 460; index++) await journal.recordPrompt(turn(index, "pending"));
  for (let index = 0; index < 80; index++) {
    await journal.appendSteer(turn(0), { id: `steer-${index}`, text: `steering-${index}`, status: "pending" });
    await journal.updateSteer(turn(0), `steer-${index}`, { status: "accepted" });
  }
  assert.equal((await reopen().get("thread", "turn-0"))?.steers?.length, 80);
  const pending: string[] = [];
  for await (const entry of reopen().pending("thread")) {
    pending.push(entry.turnId);
    // Moving rows out of the pending index between pages must not skip work.
    await journal.completeTurn({ ...entry, assistant: "recovered" });
  }
  assert.deepEqual(pending, ["turn-0", ...Array.from({ length: 160 }, (_, index) => `turn-${index + 301}`)]);
  assert.equal((await allTurns(reopen())).turns.length, 461);
  const remaining: LocalTranscriptTurn[] = [];
  for await (const entry of reopen().pending("thread")) remaining.push(entry);
  assert.deepEqual(remaining, []);
});

test("the IndexedDB upgrade normalizes rows in place without deleting old completed turns", async () => {
  const { indexedDB, databaseName, journal } = fixture();
  await new Promise<void>((resolve, reject) => {
    const request = indexedDB.open(databaseName, 3);
    request.onerror = () => reject(request.error);
    request.onupgradeneeded = () => {
      const db = request.result;
      const sessions = db.createObjectStore("sessions", { keyPath: "threadId" });
      sessions.put({ threadId: "thread", initialized: true, nextSequence: 350 });
      const turns = db.createObjectStore("turns", { keyPath: ["threadId", "turnId"] });
      turns.createIndex("thread-order", ["threadId", "order"]);
      turns.createIndex("thread-status", ["threadId", "status"]);
      for (let index = 0; index < 350; index++) {
        const { status: _, ...entry } = turn(index, index < 310 ? "completed" : "pending");
        turns.put({ ...entry, order: `~:${String(index).padStart(16, "0")}`, sequence: index });
      }
    };
    request.onsuccess = () => { request.result.close(); resolve(); };
  });
  assert.equal((await allTurns(journal)).turns.length, 350);
  const pending = [];
  for await (const entry of journal.pending("thread")) pending.push(entry.turnId);
  assert.deepEqual(pending, Array.from({ length: 40 }, (_, index) => `turn-${index + 310}`));
});
