import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { ManagedStartupContext } from "../src/startup-context";
import type { MemoryScope } from "../src/memory-scope";
import { MarkdownMemoryStore } from "../src/markdown-memory";
import { preparedMarkdownText } from "../src/markdown-memory-tools";
import { PreparedPersonalizationCache, PreparedPersonalizationStore, PERSONALIZATION_REFRESH_MS, personalizedVoiceContext,
  type PersonalizationSnapshot } from "../src/personalization";

const scope = { organization_id: "org", team_id: "team", user_id: "user" };
const storageId = "a".repeat(64);
const profile = (generation = 0): PersonalizationSnapshot => ({ ...scope, generation, version: "1:1",
  expires_at: Date.now() + 60_000, team_markdown: { documents: [{ path: "USER.md", revision: 1, content: "Prefers concise answers.", truncated: false }] } });
async function withStore(run: (store: PreparedPersonalizationStore, storage: DurableObjectStorage) => Promise<void>) {
  const ns = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> }).NANOCODEX_MEMORY;
  await runInDurableObject(ns.getByName(crypto.randomUUID()), async (_obj, ctx) => {
    await run(new PreparedPersonalizationStore(ctx.storage), ctx.storage);
  });
}
describe("prepared personalization owner", () => {
  it("reuses a scoped canonical snapshot without creating legacy fact storage", async () => {
    await withStore(async (store, storage) => {
      const markdown = new MarkdownMemoryStore(storage);
      markdown.write("team", { operation: "put", path: "USER.md", content: "concise team preference" });
      markdown.write("other-team", { operation: "put", path: "USER.md", content: "other private note" });
      const one = store.snapshot(scope, storageId)!;
      const two = store.snapshot(scope, "b".repeat(64))!;
      expect(one.team_markdown?.documents[0]?.content).toBe("concise team preference");
      expect(two.version).toBe(one.version);
      expect(storage.sql.exec("SELECT name FROM sqlite_master WHERE name IN ('durable_memories','memory_scan_receipts')").toArray()).toEqual([]);
      expect(store.snapshot({ ...scope, user_id: "different-user" }, storageId)).toBeUndefined();
    });
  });

  it("builds canonical Markdown in the background and fences copies after insert, edit, and delete", async () => {
    await withStore(async (store, storage) => {
      const markdown = new MarkdownMemoryStore(storage);
      const initial = store.snapshot(scope, storageId)!;
      expect(initial.team_markdown).toEqual({ documents: [] });
      const cache = new PreparedPersonalizationCache();
      const tasks: Promise<void>[] = [];
      cache.warm(scope, async () => initial, task => tasks.push(task));
      await tasks[0];
      markdown.write("other-team", { operation: "put", path: "USER.md", content: "other owner's private note" });
      expect(store.invalidationPending()).toBe(false);
      expect(store.snapshot(scope, storageId)?.generation).toBe(initial.generation);

      markdown.write(scope.team_id, { operation: "put", path: "USER.md", content: "original preference" });
      expect(storage.sql.exec<{ body_json: string | null }>(
        "SELECT body_json FROM prepared_personalization WHERE team_id=?", scope.team_id).one().body_json).toBeNull();
      expect(store.invalidationPending()).toBe(true);
      const notify = vi.fn(async (_id: string, value: { generation: number }) => cache.invalidate(value.generation));
      await store.invalidate(notify);
      expect(cache.peek(scope)).toBeUndefined();
      const created = store.snapshot(scope, storageId)!;
      expect(created.team_markdown?.documents).toEqual([
        { path: "USER.md", revision: 1, content: "original preference", truncated: false },
      ]);
      expect(created.user_markdown).toBeUndefined();
      expect(created.generation).toBeGreaterThan(initial.generation);
      expect(created.version).not.toBe(initial.version);
      expect(notify).toHaveBeenCalledWith(storageId, { team_id: scope.team_id, user_id: scope.user_id, generation: created.generation });
      // A stale in-flight response cannot restore the pre-edit Markdown profile.
      cache.warm(scope, async () => initial, task => tasks.push(task));
      await tasks[1];
      expect(cache.peek(scope)).toBeUndefined();

      markdown.write(scope.team_id, { operation: "put", path: "USER.md", expected_revision: 1, content: "corrected preference" });
      const edited = store.snapshot(scope, storageId)!;
      expect(edited.team_markdown?.documents[0]).toMatchObject({ revision: 2, content: "corrected preference" });
      expect(edited.generation).toBeGreaterThan(created.generation);
      expect(edited.version).not.toBe(created.version);
      markdown.write(scope.team_id, { operation: "delete", path: "USER.md", expected_revision: 2 });
      const deleted = store.snapshot(scope, storageId)!;
      expect(deleted.team_markdown).toEqual({ documents: [] });
      expect(deleted.generation).toBeGreaterThan(edited.generation);
      expect(deleted.version).not.toBe(edited.version);
      expect(store.invalidationPending()).toBe(true);
      await store.invalidate(notify);
      expect(notify).toHaveBeenLastCalledWith(storageId, { team_id: scope.team_id, user_id: scope.user_id, generation: deleted.generation });
      expect(store.invalidationPending()).toBe(false);

      // Physical canonical removal is fenced too, in addition to write's tombstones.
      markdown.write(scope.team_id, { operation: "put", path: "MEMORY.md", content: "removed canonical row" });
      const beforeRemoval = store.snapshot(scope, storageId)!;
      storage.sql.exec("DELETE FROM markdown_memory_documents WHERE owner=? AND path='MEMORY.md'", scope.team_id);
      const removed = store.snapshot(scope, storageId)!;
      expect(removed.team_markdown).toEqual({ documents: [] });
      expect(removed.generation).toBeGreaterThan(beforeRemoval.generation);
      expect(store.invalidationPending()).toBe(true);
    });
  });

  it("expires Markdown snapshots at the UTC day boundary and changes identity with the daily window", async () => {
    await withStore(async (store, storage) => {
      const markdown = new MarkdownMemoryStore(storage);
      const midnight = Date.parse("2026-09-24T00:00:00Z");
      markdown.write(scope.team_id, { operation: "put", path: "memory/2026-09-22.md", content: "older daily note" });
      markdown.write(scope.team_id, { operation: "put", path: "memory/2026-09-24.md", content: "new daily note" });
      markdown.write(scope.team_id, { operation: "put", path: "DREAMS.md", content: "excluded consolidation journal" });
      const before = store.snapshot(scope, storageId, midnight - 1_000)!;
      expect(before.expires_at).toBe(midnight);
      expect(before.team_markdown?.documents.map(doc => doc.path)).toEqual(["memory/2026-09-22.md"]);
      const after = store.snapshot(scope, storageId, midnight)!;
      expect(after.team_markdown?.documents.map(doc => doc.path)).toEqual(["memory/2026-09-24.md"]);
      expect(after.generation).toBe(before.generation);
      expect(after.version).not.toBe(before.version);
      expect(after.expires_at).toBe(midnight + PERSONALIZATION_REFRESH_MS);
    });
  });

  it("keeps Markdown identity fixed-size and never renders retired fields from an old receipt", async () => {
    await withStore(async (store, storage) => {
      const markdown = new MarkdownMemoryStore(storage);
      const now = Date.parse("2026-09-23T12:00:00Z");
      const initial = store.snapshot(scope, storageId, now)!;
      for (let i = 0; i < 4; i++) markdown.write(scope.team_id, {
        operation: "put", path: `memory/2026-09-23-${"topic".repeat(20)}-${i}.md`, content: `daily note ${i}`,
      });
      const current = store.snapshot(scope, storageId, now + 1)!;
      expect(current.team_markdown?.documents).toHaveLength(4);
      expect(current.version).toMatch(/^markdown:[a-f0-9]{64}$/);
      expect(current.version.length).toBe(initial.version.length);
      expect(current.version).not.toBe(initial.version);
      const retained = { ...current, team_facts: [{ content: "retired fact canary" }], user_facts: [{ content: "private retired canary" }] };
      const text = preparedMarkdownText(retained)!;
      expect(personalizedVoiceContext({}, retained).markdown_memory).toBe(text);
      expect(personalizedVoiceContext({}, retained).prepared_personalization).toBeUndefined();
      expect(text).not.toContain("canary");
      expect(text).not.toContain("team_facts");
      expect(text).not.toContain("user_facts");
    });
  });

  it("upgrades retained fact-only profiles on the next background snapshot request", async () => {
    await withStore(async (store, storage) => {
      const markdown = new MarkdownMemoryStore(storage);
      markdown.write(scope.team_id, { operation: "put", path: "MEMORY.md", content: "saved before deployment" });
      const current = store.snapshot(scope, storageId)!;
      storage.sql.exec("UPDATE prepared_personalization SET body_json=? WHERE team_id=?",
        JSON.stringify({ version: "empty", generation: current.generation, valid_until: Date.now() + PERSONALIZATION_REFRESH_MS, team_facts: [] }), scope.team_id);
      const upgraded = store.snapshot(scope, storageId)!;
      expect(upgraded.team_markdown?.documents[0]?.content).toBe("saved before deployment");
      expect(upgraded.version).not.toBe("empty");
    });
  });

  it("retains invalidation debt until every issued live copy acknowledges", async () => {
    await withStore(async (store, storage) => {
      const markdown = new MarkdownMemoryStore(storage);
      markdown.write("team", { operation: "put", path: "USER.md", content: "old preference" });
      store.snapshot(scope, storageId); store.snapshot(scope, "b".repeat(64));
      markdown.write("team", { operation: "delete", path: "USER.md" });
      const notify = vi.fn(async (id: string) => { if (id === storageId) throw new Error("offline"); });
      await expect(store.invalidate(notify)).rejects.toThrow("invalidation pending");
      expect(store.invalidationPending()).toBe(true);
      expect(notify).toHaveBeenCalledTimes(2);
      const retry = vi.fn(async () => {});
      await store.invalidate(retry);
      expect(retry).toHaveBeenCalledTimes(1);
      expect(store.invalidationPending()).toBe(false);
    });
  });
});

describe("nonblocking local personalization", () => {
  it("never waits for a cold/stalled refresh; shares one in-flight refresh", async () => {
    const cache = new PreparedPersonalizationCache();
    let release!: (value: PersonalizationSnapshot) => void;
    const pending = new Promise<PersonalizationSnapshot>(resolve => { release = resolve; });
    const load = vi.fn(() => pending); const tasks: Promise<void>[] = [];
    cache.warm(scope, load, task => tasks.push(task));
    cache.warm(scope, load, task => tasks.push(task));
    expect(cache.peek(scope)).toBeUndefined();
    expect(tasks).toHaveLength(1);
    await Promise.resolve(); expect(load).toHaveBeenCalledOnce();
    release(profile()); await tasks[0];
    expect(cache.peek(scope)?.team_markdown?.documents[0]?.content).toContain("concise");
    expect(cache.peek({ ...scope, team_id: "other" })).toBeUndefined();
  });

  it("fences a forgotten snapshot even if an older response arrives late", async () => {
    const cache = new PreparedPersonalizationCache();
    let release!: (value: PersonalizationSnapshot) => void;
    const tasks: Promise<void>[] = [];
    cache.warm(scope, () => new Promise(resolve => { release = resolve; }), task => tasks.push(task));
    await Promise.resolve();
    cache.invalidate(2); release(profile(1)); await tasks[0];
    expect(cache.peek(scope)).toBeUndefined();
    cache.warm(scope, async () => profile(2), task => tasks.push(task));
    await tasks[1]; expect(cache.peek(scope)?.generation).toBe(2);
    expect(cache.peek(scope, Date.now() + 60_001)).toBeUndefined();
  });

  it("turns refresh failure into a miss rather than a prompt failure", async () => {
    const cache = new PreparedPersonalizationCache(); const tasks: Promise<void>[] = [];
    cache.warm(scope, async () => { throw new Error("unavailable"); }, task => tasks.push(task));
    await expect(tasks[0]).resolves.toBeUndefined();
    expect(cache.peek(scope)).toBeUndefined();
  });
});


describe("MemoryScope to Session invalidation", () => {
  it("background forgetting drops the real subscribed Session pending copy", async () => {
    const bindings = env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope>;
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> };
    const memory = bindings.NANOCODEX_MEMORY.getByName(crypto.randomUUID());
    const session = bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID());
    await runInDurableObject(session, async (_obj, ctx) => {
      ctx.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
        authorization_epoch,public_origin,runtime_profile,accepted_turns,last_active)
        VALUES(1,?,'user','org','team',1,'https://test.example','managed',0,?)`, crypto.randomUUID(), Date.now());
    });
    const headers = { "x-nanocodex-organization-id": "org", "x-nanocodex-team-id": "team",
      "x-nanocodex-memory-initialize": "1", "x-nanocodex-subject-id": "fixture",
      "x-nanocodex-memory-mutation": "1" };
    expect((await memory.fetch("https://memory.internal/initialize", { method: "PUT", headers })).status).toBe(204);
    const put = await memory.fetch("https://memory.internal/markdown-memory/write", { method: "POST", headers,
      body: JSON.stringify({ operation: "put", path: "USER.md", content: "Disposable personalization canary" }) });
    expect(put.status).toBe(200);
    const response = await memory.fetch("https://memory.internal/personalization", { method: "POST",
      headers: { ...headers, "x-nanocodex-personalization-user": "user",
        "x-nanocodex-personalization-session": session.id.toString() } });
    expect(response.status).toBe(200);
    const { snapshot } = await response.json<{ snapshot: PersonalizationSnapshot }>();
    await runInDurableObject(session, async (_obj, ctx) => {
      new ManagedStartupContext(ctx.storage).reservePrepared("pending", snapshot, false);
    });
    const forgotten = await memory.fetch("https://memory.internal/markdown-memory/write", { method: "POST", headers,
      body: JSON.stringify({ operation: "delete", path: "USER.md" }) });
    expect(forgotten.status).toBe(200);
    await runInDurableObject(memory, async object => { await object.alarm(); });
    await runInDurableObject(session, async (_obj, ctx) => {
      expect(ctx.storage.sql.exec<{ profile_json: string | null }>(
        "SELECT profile_json FROM managed_prepared_personalization WHERE turn_id='pending'").one().profile_json).toBeNull();
    });
  });
});


it("reprojects voice replay context so a durable receipt cannot resurrect a forgotten profile", () => {
  const receipt = { history: [], prepared_personalization: "forgotten canary", markdown_memory: "deleted USER.md" };
  expect(personalizedVoiceContext(receipt)).toEqual({ history: [] });
  const refreshed = personalizedVoiceContext(receipt, { ...profile(2), team_markdown: { documents: [
    { path: "USER.md", revision: 2, content: "current USER.md", truncated: false },
  ] } });
  expect(refreshed.prepared_personalization).toBeUndefined();
  expect(JSON.stringify(refreshed)).not.toContain("forgotten canary");
  expect(refreshed.markdown_memory).toContain("current USER.md");
  expect(refreshed.markdown_memory).not.toContain("deleted USER.md");
  expect(receipt.markdown_memory).toBe("deleted USER.md");
});

it("normal and voice keep private and team Markdown separate without rendering legacy fields", () => {
  const value: PersonalizationSnapshot = { ...profile(),
    user_markdown: { documents: [{ path: "USER.md", revision: 2, content: "private canary", truncated: false }] },
    user_generation: 1, user_version: "markdown:private" };
  const voice = personalizedVoiceContext({}, value);
  expect(voice.prepared_personalization).toBeUndefined();
  expect(voice.markdown_memory).toBe(preparedMarkdownText(value));
  expect(voice.markdown_memory).toContain("private canary");
  expect(voice.markdown_memory).toContain("concise answers");
  expect(voice.markdown_memory).toContain('"scope":"personal"');
  expect(voice.markdown_memory).toContain('"scope":"team"');
});
