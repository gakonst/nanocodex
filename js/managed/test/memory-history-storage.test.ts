import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { MemoryScope } from "../src/memory-scope";
import { initializeHistoryStorage, readHistoryText, storeHistorySegments } from "../src/memory-history-storage";

const bindings = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> };
const headers = { "x-nanocodex-organization-id": "organization", "x-nanocodex-team-id": "team" };
const threadId = "01a08669-3851-7d44-af51-cacb3f44b9d8";
const project = (input: string, final_message: string, cursor = "10") => ({
  thread_id: threadId, turn_id: "turn", cursor, input, final_message, title: "Large history", created_at: 1_000,
});
async function request(memory: MemoryScope, path: string, body: unknown, team = "team") {
  return memory.fetch(new Request(`https://memory.internal${path}`, {
    method: "POST", headers: { ...headers, "x-nanocodex-team-id": team }, body: JSON.stringify(body),
  }));
}

describe("segmented history projection", () => {
  it("projects >2 MiB user and answer text, searches its tail and restores exact text", async () => {
    await runInDurableObject(bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (memory, state) => {
      await memory.fetch(new Request("https://memory.internal/initialize", { method: "PUT", headers }));
      const user = "copper ".repeat(310_000) + "😀 user-tail-exact-492";
      const answer = "lighthouse ".repeat(200_000) + " answer-tail-exact-831";
      expect((await request(memory, "/project", project(user, answer))).status).toBe(204);
      expect(state.storage.sql.exec<{ bytes: number }>(
        "SELECT MAX(LENGTH(CAST(content AS BLOB))) AS bytes FROM memory_segments",
      ).one().bytes).toBeLessThan(1_040_000);
      expect(state.storage.sql.exec<{ name: string }>("PRAGMA table_info(memory_turns)").toArray()
        .map((column) => column.name)).not.toContain("content");
      const read = await request(memory, "/read", { session_id: threadId });
      expect(read.status).toBe(200);
      expect(await read.json()).toMatchObject({ turns: [{ user, assistant: answer }] });
      for (const query of ["user-tail-exact-492", "answer-tail-exact-831", "copper lighthouse missingword"]) {
        const searched = await request(memory, "/search", { query, limit: 5 });
        expect(searched.status).toBe(200);
        expect(await searched.json()).toMatchObject({ results: [{ turn_id: "turn", cursor: "10" }] });
      }
      expect(await (await request(memory, "/search", { query: "answer-tail-exact-831" }, "other-team")).json())
        .toMatchObject({ results: [] });
      const ids = state.storage.sql.exec<{ segment_id: string }>("SELECT segment_id FROM memory_segments").toArray();
      expect((await request(memory, "/project", project("stale", "stale", "9"))).status).toBe(204);
      expect((await request(memory, "/project", project("replayed", "replayed", "10"))).status).toBe(204);
      expect(state.storage.sql.exec("SELECT segment_id FROM memory_segments").toArray()).toEqual(ids);
      expect((await request(memory, "/project", project("new prompt", "new answer", "11"))).status).toBe(204);
      expect(await (await request(memory, "/read", { session_id: threadId })).json())
        .toMatchObject({ turns: [{ user: "new prompt", assistant: "new answer", cursor: "11" }] });
      expect(await (await request(memory, "/search", { query: "answer-tail-exact-831" })).json())
        .toMatchObject({ results: [] });
      expect((await memory.fetch(new Request(`https://memory.internal/threads/${threadId}`, { method: "DELETE", headers }))).status).toBe(204);
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM memory_segments").one().n).toBe(0);
      expect((await request(memory, "/project", project(user, answer, "12"))).status).toBe(204);
      expect(await (await request(memory, "/read", { session_id: threadId })).json()).toMatchObject({ turns: [] });
    });
  });

  it("finds identifiers and phrases crossing chunk boundaries without duplicate turns", async () => {
    await runInDurableObject(bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (memory) => {
      await memory.fetch(new Request("https://memory.internal/initialize", { method: "PUT", headers }));
      const input = " ".repeat(255_992) + "unique-identifier-274 copper lighthouse";
      expect((await request(memory, "/project", project(input, "done"))).status).toBe(204);
      for (const query of ["unique-identifier-274", "copper lighthouse"]) {
        const data = await (await request(memory, "/search", { query })).json() as { results: unknown[] };
        expect(data.results).toHaveLength(1);
      }
    });
  });

  it("migrates inline rows and pending AI work atomically to exact segments and reference-only jobs", async () => {
    await runInDurableObject(bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (_memory, state) => {
      state.storage.sql.exec(`
        DROP TRIGGER memory_segments_ai; DROP TRIGGER memory_segments_ad;
        DROP TABLE memory_segments_fts; DROP TABLE memory_segments; DROP TABLE memory_turns; DROP TABLE memory_ai_outbox;
        CREATE TABLE memory_turns (segment_id TEXT PRIMARY KEY, thread_id TEXT, turn_id TEXT,
          source_cursor INTEGER, user_text TEXT, assistant_text TEXT, content TEXT, created_at INTEGER, ai_item_id TEXT);
        CREATE TABLE memory_ai_outbox (operation_id TEXT, operation TEXT, segment_id TEXT, payload_json TEXT, ai_item_id TEXT);
        INSERT INTO memory_threads VALUES ('thread', 'team', 'title', 1, 1);
      `);
      const user = "original 😀 ".repeat(30_000);
      const assistant = "tail";
      state.storage.sql.exec("INSERT INTO memory_turns VALUES ('turn', 'thread', 'turn', 10, ?, ?, ?, 1, 'old-ai')",
        user, assistant, `User: ${user}\n\nAssistant: ${assistant}`);
      state.storage.sql.exec("INSERT INTO memory_ai_outbox VALUES ('delete:old', 'delete', 'old', NULL, 'deleted-ai')");
      initializeHistoryStorage(state.storage, true);
      expect(readHistoryText(state.storage, "turn", "user")).toBe(user);
      expect(readHistoryText(state.storage, "turn", "assistant")).toBe(assistant);
      expect(state.storage.sql.exec<{ name: string }>("PRAGMA table_info(memory_ai_outbox)").toArray()
        .map((column) => column.name)).not.toContain("payload_json");
      expect(state.storage.sql.exec("SELECT operation_id, ai_item_id FROM memory_ai_outbox WHERE operation = 'delete' ORDER BY operation_id").toArray())
        .toEqual([{ operation_id: "delete:old", ai_item_id: "deleted-ai" }, { operation_id: "delete:turn", ai_item_id: "old-ai" }]);
      initializeHistoryStorage(state.storage, true);
      expect(readHistoryText(state.storage, "turn", "user")).toBe(user);
      // New very large content adds only small reference rows to the AI outbox.
      state.storage.transactionSync(() => {
        state.storage.sql.exec("INSERT INTO memory_turns VALUES ('big', 'thread', 'big', 20, 2)");
        storeHistorySegments(state.storage, "big", "x".repeat(2_200_000), "y".repeat(2_200_000), true);
      });
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM memory_ai_outbox WHERE operation = 'upsert'").one().n).toBeGreaterThan(18);
      expect(readHistoryText(state.storage, "big", "assistant")).toBe("y".repeat(2_200_000));
      await state.storage.deleteAlarm();
    });
  });
  it("bounds AI upload bodies and does not lose new work when a projection replaces an in-flight upload", async () => {
    await runInDurableObject(bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (memory, state) => {
      await memory.fetch(new Request("https://memory.internal/initialize", { method: "PUT", headers }));
      let release!: () => void;
      let started!: () => void;
      const firstStarted = new Promise<void>((resolve) => { started = resolve; });
      const firstRelease = new Promise<void>((resolve) => { release = resolve; });
      const uploaded = new Map<string, { id: string; key: string; content: string; metadata: Record<string, unknown> }>();
      const disposable = <T extends object>(value: T) => ({ ...value, [Symbol.dispose]() {} });
      let first = true;
      const items = {
        async upload(key: string, content: string, options: { metadata: Record<string, unknown> }) {
          expect(new TextEncoder().encode(content).length).toBeLessThan(1_040_000);
          const id = crypto.randomUUID();
          if (first) { first = false; started(); await firstRelease; }
          uploaded.set(id, { id, key, content, metadata: options.metadata });
          return disposable({ id, status: "completed" });
        },
        async list({ key }: { key: string }) {
          return disposable({ result: [...uploaded.values()].filter((item) => item.key === key) });
        },
        async delete(id: string) { uploaded.delete(id); },
        get(id: string) { return disposable({ async info() { return disposable({ id, status: "completed" }); } }); },
      };
      Object.defineProperty(memory, "env", { value: { HISTORY_AI_SEARCH: { items } } });
      expect((await request(memory, "/project", project("old ".repeat(550_000), "old answer"))).status).toBe(204);
      await firstStarted;
      const oldSegments = state.storage.sql.exec<{ segment_id: string }>("SELECT segment_id FROM memory_segments").toArray();
      expect((await request(memory, "/project", project("new prompt", "new answer", "11"))).status).toBe(204);
      release();
      await memory.alarm();
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM memory_ai_outbox WHERE operation = 'upsert'").one().n).toBe(0);
      const retained = state.storage.sql.exec<{ ai_item_id: string }>("SELECT ai_item_id FROM memory_segments").toArray();
      expect(retained).toHaveLength(2);
      expect(retained.every((row) => uploaded.has(row.ai_item_id))).toBe(true);
      for (let pass = 0; pass < 5; pass++) {
        state.storage.sql.exec("UPDATE memory_ai_outbox SET retry_at = 0");
        await memory.alarm();
      }
      expect([...uploaded.values()].some((item) => oldSegments.some((old) => item.key === `${old.segment_id}.md`))).toBe(false);
      expect(uploaded.size).toBe(2);
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM memory_ai_outbox").one().n).toBe(0);
      await state.storage.deleteAlarm();
    });
  });

  it("migrates a thousand 64 KiB legacy turns without hydrating the whole scope", async () => {
    await runInDurableObject(bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID()), async (_memory, state) => {
      state.storage.sql.exec(`
        DROP TRIGGER memory_segments_ai; DROP TRIGGER memory_segments_ad;
        DROP TABLE memory_segments_fts; DROP TABLE memory_segments; DROP TABLE memory_turns; DROP TABLE memory_ai_outbox;
        CREATE TABLE memory_turns (segment_id TEXT PRIMARY KEY, thread_id TEXT, turn_id TEXT,
          source_cursor INTEGER, user_text TEXT, assistant_text TEXT, content TEXT, created_at INTEGER, ai_item_id TEXT);
        CREATE TABLE memory_ai_outbox (operation_id TEXT, operation TEXT, segment_id TEXT, payload_json TEXT, ai_item_id TEXT);
        INSERT INTO memory_threads VALUES ('thread', 'team', 'title', 1, 1);
      `);
      const user = "migration copper lighthouse ".repeat(1_214);
      const answer = "durable violet train ".repeat(1_638);
      state.storage.transactionSync(() => {
        for (let index = 0; index < 1_000; index++) state.storage.sql.exec(
          "INSERT INTO memory_turns VALUES (?, 'thread', ?, ?, ?, ?, ?, 1, NULL)",
          `turn-${index}`, `turn-${index}`, index, user, answer, `User: ${user} Assistant: ${answer}`,
        );
      });
      const start = performance.now();
      initializeHistoryStorage(state.storage, true);
      console.log(`history migration: 1000 turns / ${((user.length + answer.length) * 1_000 / 1024 / 1024).toFixed(1)} MiB content in ${(performance.now() - start).toFixed(0)} ms`);
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM memory_turns").one().n).toBe(1_000);
      expect(readHistoryText(state.storage, "turn-999", "assistant")).toBe(answer);
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM memory_ai_outbox WHERE operation = 'upsert'").one().n).toBe(2_000);
    });
  }, 120_000);

});
