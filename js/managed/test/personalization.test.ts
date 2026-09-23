import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { ManagedStartupContext } from "../src/startup-context";
import type { MemoryScope } from "../src/memory-scope";
import { storeMemoryContent } from "../src/durable-memory-storage";
import { PreparedPersonalizationCache, PreparedPersonalizationStore, PERSONALIZATION_REFRESH_MS, personalizedVoiceContext, personalizationText,
  type PersonalizationSnapshot } from "../src/personalization";

const scope = { organization_id: "org", team_id: "team", user_id: "user" };
const storageId = "a".repeat(64);
const profile = (generation = 0): PersonalizationSnapshot => ({ ...scope, generation, version: "1:1",
  expires_at: Date.now() + 60_000, team_facts: [{ id: 1, version: 1, content: "Prefers concise answers." }] });
async function withStore(run: (store: PreparedPersonalizationStore, storage: DurableObjectStorage) => Promise<void>) {
  const ns = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> }).NANOCODEX_MEMORY;
  await runInDurableObject(ns.getByName(crypto.randomUUID()), async (_obj, ctx) => {
    await run(new PreparedPersonalizationStore(ctx.storage), ctx.storage);
  });
}
function add(storage: DurableObjectStorage, id: number, team = "team", content = `fact ${id}`, expires: number | null = null) {
  storage.sql.exec(`INSERT INTO durable_memories(id,version,owner_team_id,content_json,identity_digest,
    created_at_ms,updated_at_ms,probation_until_ms) VALUES (?,1,?,'',?,1,?,?)`, id, team, `digest-${id}`, id, expires);
  storeMemoryContent(storage, id, content);
}

describe("prepared personalization owner", () => {
  it("reuses a bounded, stable saved-fact snapshot across agents without query ranking or scan writes", async () => {
    await withStore(async (store, storage) => {
      add(storage, 1); add(storage, 2, "other-team", "private other team");
      const one = store.snapshot(scope, storageId)!;
      const two = store.snapshot(scope, "b".repeat(64))!;
      expect(one.team_facts).toEqual([{ id: 1, version: 1, content: "fact 1" }]);
      expect(two.version).toBe(one.version);
      expect(storage.sql.exec<{ scan_count: number; use_count: number }>("SELECT scan_count,use_count FROM durable_memories WHERE id=1").one())
        .toEqual({ scan_count: 0, use_count: 0 });
      expect(storage.sql.exec("SELECT * FROM memory_scan_receipts").toArray()).toEqual([]);
      expect(store.snapshot({ ...scope, user_id: "different-user" }, storageId)).toBeUndefined();
    });
  });

  it("coalesces additions but rebuilds replaced and forgotten facts immediately", async () => {
    await withStore(async (store, storage) => {
      const now = Date.now(); add(storage, 1);
      const initial = store.snapshot(scope, storageId, now)!;
      add(storage, 2);
      expect(store.snapshot(scope, storageId, now + 1)?.version).toBe(initial.version);
      const refreshed = store.snapshot(scope, storageId, now + PERSONALIZATION_REFRESH_MS + 1)!;
      expect(refreshed.team_facts.map(f => f.id)).toEqual([1, 2]);
      storage.sql.exec("UPDATE durable_memories SET version=2 WHERE id=1");
      storeMemoryContent(storage, 1, "corrected fact");
      expect(store.snapshot(scope, storageId, now + PERSONALIZATION_REFRESH_MS + 2)?.team_facts[0]?.content).toBe("corrected fact");
      storage.sql.exec("DELETE FROM durable_memories WHERE id=1");
      const after = store.snapshot(scope, storageId, now + PERSONALIZATION_REFRESH_MS + 3)!;
      expect(after.team_facts.map(f => f.id)).toEqual([2]);
      expect(after.generation).toBeGreaterThan(refreshed.generation);
    });
  });

  it("bounds profile contents and the lease of expiring source facts", async () => {
    await withStore(async (store, storage) => {
      const now = Date.now();
      for (let i = 1; i <= 40; i++) add(storage, i, "team", "x".repeat(1_000));
      add(storage, 41, "team", "expires soon", now + 1_000);
      const value = store.snapshot(scope, storageId, now)!;
      expect(value.team_facts.length).toBeLessThanOrEqual(32);
      expect(new TextEncoder().encode(JSON.stringify(value.team_facts)).byteLength).toBeLessThan(8_100);
      expect(value.expires_at).toBe(now + 1_000);
      expect(store.snapshot(scope, storageId, now + 1_001)?.team_facts.some(f => f.id === 41)).toBe(false);
    });
  });

  it("retains invalidation debt until every issued live copy acknowledges", async () => {
    await withStore(async (store, storage) => {
      add(storage, 1); store.snapshot(scope, storageId); store.snapshot(scope, "b".repeat(64));
      storage.sql.exec("DELETE FROM durable_memories WHERE id=1");
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
    expect(cache.peek(scope)?.team_facts[0]?.content).toContain("concise");
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
  it("acknowledges forget only after the real subscribed Session drops its pending copy", async () => {
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
    expect((await memory.fetch("https://memory.internal/memory", { method: "POST", headers,
      body: JSON.stringify({ operation: "scan", query: "Disposable personalization canary" }) })).status).toBe(200);
    const put = await memory.fetch("https://memory.internal/memory", { method: "POST", headers,
      body: JSON.stringify({ operation: "put", content: "Disposable personalization canary" }) });
    expect(put.status).toBe(200);
    const saved = await put.json<{ memory: { key: { id: number; version: number } } }>();
    const response = await memory.fetch("https://memory.internal/personalization", { method: "POST",
      headers: { ...headers, "x-nanocodex-personalization-user": "user",
        "x-nanocodex-personalization-session": session.id.toString() } });
    expect(response.status).toBe(200);
    const { snapshot } = await response.json<{ snapshot: PersonalizationSnapshot }>();
    await runInDurableObject(session, async (_obj, ctx) => {
      new ManagedStartupContext(ctx.storage).reservePrepared("pending", snapshot, false);
    });
    const forgotten = await memory.fetch("https://memory.internal/memory", { method: "POST", headers,
      body: JSON.stringify({ operation: "delete", key: saved.memory.key }) });
    expect(forgotten.status).toBe(200);
    await runInDurableObject(session, async (_obj, ctx) => {
      expect(ctx.storage.sql.exec<{ profile_json: string | null }>(
        "SELECT profile_json FROM managed_prepared_personalization WHERE turn_id='pending'").one().profile_json).toBeNull();
    });
  });
});


it("reprojects voice replay context so a durable receipt cannot resurrect a forgotten profile", () => {
  const receipt = { history: [], prepared_personalization: "forgotten canary", markdown_memory: "deleted USER.md" };
  expect(personalizedVoiceContext(receipt)).toEqual({ history: [] });
  const refreshed = personalizedVoiceContext(receipt, profile(2), "current USER.md");
  expect(refreshed.prepared_personalization).toContain("concise answers");
  expect(refreshed.prepared_personalization).not.toContain("forgotten canary");
  expect(refreshed.markdown_memory).toBe("current USER.md");
  expect(receipt.markdown_memory).toBe("deleted USER.md");
});

it("normal and voice retain both prepared scopes within the rendered budget after escaping", () => {
  const value = { ...profile(), team_facts: [{ id: 1, version: 1, content: `team ${"<&🦊".repeat(1_200)}` }],
    user_facts: [{ id: 2, version: 1, content: `personal ${"<&🦊".repeat(1_200)}` }], user_generation: 1, user_version: "2:1" };
  const text = personalizationText(value);
  expect(personalizedVoiceContext({}, value).prepared_personalization).toBe(text);
  expect(new TextEncoder().encode(text).byteLength).toBeLessThan(20_000);
  expect(text).toContain('"content":"personal');
  expect(text).toContain('"content":"team');
  expect(text).toContain('"truncated":true');
  expect(text).not.toContain("\ufffd");
});
