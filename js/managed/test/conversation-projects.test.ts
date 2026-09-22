import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { retireAccountProjects } from "../src/retired-projects";

const root = "11111111-1111-4111-8111-111111111111";
const child = "22222222-2222-4222-8222-222222222222";
const independent = "33333333-3333-4333-8333-333333333333";
type Registry = { fetch(request: Request): Promise<Response> };
async function inside(test: (storage: DurableObjectStorage, registry: Registry) => Promise<void>) {
  const ns = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
  await runInDurableObject(ns.getByName(crypto.randomUUID()), async (instance, state) => {
    for (const id of [root, child, independent]) {
      state.storage.sql.exec("INSERT INTO agent_registry(id,title,created_at,updated_at,turn_count) VALUES (?,?,1,2,3)", id, `Original ${id}`);
    }
    // An existing production registry from before retirement, including its backup.
    state.storage.sql.exec(`CREATE TABLE project_threads (agent_id TEXT PRIMARY KEY,parent_agent_id TEXT,project_root_id TEXT);
      CREATE TABLE conversation_projects (agent_id TEXT PRIMARY KEY,project_root_id TEXT,project_name TEXT);
      CREATE TABLE conversation_project_migration (id TEXT PRIMARY KEY,backup_json TEXT);
      INSERT INTO conversation_project_migration VALUES ('old-migration','{"assignments":[]}');`);
    state.storage.sql.exec("INSERT INTO project_threads VALUES (?,?,?)", child, root, root);
    state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", root, root, "Project Alpha");
    state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", independent, root, "Project Alpha");
    await test(state.storage, instance as unknown as Registry);
  });
}

describe("retired conversation projects", () => {
  it("undoes migrated grouping idempotently without changing conversations or the original backup", () => inside(async (storage, registry) => {
    const before = storage.sql.exec("SELECT * FROM agent_registry ORDER BY id").toArray();
    const backup = storage.sql.exec("SELECT * FROM conversation_project_migration").toArray();
    retireAccountProjects(storage);
    retireAccountProjects(storage);
    expect(storage.sql.exec("SELECT * FROM agent_registry ORDER BY id").toArray()).toEqual(before);
    expect(storage.sql.exec("SELECT * FROM conversation_project_migration").toArray()).toEqual(backup);
    expect(storage.sql.exec("SELECT name FROM sqlite_master WHERE name IN ('project_threads','conversation_projects')").toArray()).toEqual([]);
    const roster = await (await registry.fetch(new Request("https://user.internal/agents"))).json<Array<Record<string, unknown>>>();
    expect(roster).toHaveLength(3);
    for (const item of roster) {
      expect(item.title).toBe(`Original ${item.id}`);
      expect(item.turnCount).toBe(3);
      expect(item).not.toHaveProperty("projectRootId");
      expect(item).not.toHaveProperty("parentAgentId");
    }
  }));

  it("does not expose retired membership reads, writes, or migration endpoints", () => inside(async (storage, registry) => {
    retireAccountProjects(storage);
    for (const path of [`project-threads/${root}`, "conversation-project-migration-20000101"]) {
      for (const method of ["GET", "POST"]) {
        expect((await registry.fetch(new Request(`https://user.internal/${path}`, { method }))).status).toBe(404);
      }
    }
  }));
});

import { retireSessionProjects, isRetiredProjectCompletion } from "../src/retired-projects";

it("retires old outboxes and cancels ledger-owned notifications without touching user turns or broadening tools", () => inside(async storage => {
  storage.sql.exec(`CREATE TABLE managed_turns (id TEXT PRIMARY KEY, state TEXT);
    CREATE TABLE project_thread_runs (id TEXT PRIMARY KEY);
    CREATE TABLE project_spawn_plans (id TEXT PRIMARY KEY);
    CREATE TABLE managed_configuration (singleton INTEGER PRIMARY KEY,body TEXT);
    INSERT INTO project_thread_runs VALUES ('pending'),('done');
    INSERT INTO managed_turns VALUES ('project-result:pending','accepted'),('project-result:done','completed'),('project-result:user','accepted'),('ordinary','accepted');`);
  storage.sql.exec("INSERT INTO managed_configuration VALUES (1,?)", JSON.stringify({ tools: ["spawn_project_thread", "send_project_thread"], multi_agent: { enabled: false } }));
  const cancelled: string[] = [];
  const cancel = (id: string) => { cancelled.push(id); storage.sql.exec("UPDATE managed_turns SET state='cancelling' WHERE id=?", id); };
  retireSessionProjects(storage, cancel);
  retireSessionProjects(storage, cancel);
  expect(cancelled).toEqual(["project-result:pending"]);
  expect(isRetiredProjectCompletion(storage, "project-result:pending")).toBe(true);
  expect(isRetiredProjectCompletion(storage, "project-result:done")).toBe(true);
  expect(isRetiredProjectCompletion(storage, "project-result:user")).toBe(false);
  expect(storage.sql.exec("SELECT name FROM sqlite_master WHERE name IN ('project_thread_runs','project_spawn_plans')").toArray()).toEqual([]);
  expect(storage.sql.exec("SELECT id FROM managed_turns WHERE state='accepted' ORDER BY id").toArray()).toEqual([{ id: "ordinary" }, { id: "project-result:user" }]);
  const config = storage.sql.exec<{ body: string }>("SELECT body FROM managed_configuration").one();
  expect(JSON.parse(config.body)).toEqual({ tools: [], multi_agent: { enabled: false } });
}));
