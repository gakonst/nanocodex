import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { initializeConversationProjects } from "../src/conversation-projects";

const root = "11111111-1111-4111-8111-111111111111";
const child = "22222222-2222-4222-8222-222222222222";
const independent = "33333333-3333-4333-8333-333333333333";
type Registry = { fetch(request: Request): Promise<Response> };
async function inside(test: (storage: DurableObjectStorage, registry: Registry) => Promise<void>) {
  const ns = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
  await runInDurableObject(ns.getByName(crypto.randomUUID()), async (instance, state) => {
    for (const id of [root, child, independent]) {
      state.storage.sql.exec("INSERT INTO agent_registry(id,title,created_at,updated_at) VALUES (?,?,1,1)", id, "Original title");
    }
    state.storage.sql.exec("INSERT INTO project_threads(agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at) VALUES (?,?,?,?,?,?,?,1)",
      child, root, root, "request-1", "project:task", "Example task", "a".repeat(64));
    state.storage.sql.exec("INSERT INTO conversation_projects(agent_id,project_root_id,project_name) VALUES (?,?,?)", root, root, "Project Alpha");
    await test(state.storage, instance as unknown as Registry);
  });
}

describe("stored conversation projects", () => {
  it("preserves existing membership and execution links on schema initialization", () => inside(async storage => {
    const members = storage.sql.exec("SELECT * FROM conversation_projects ORDER BY agent_id").toArray();
    const links = storage.sql.exec("SELECT * FROM project_threads ORDER BY agent_id").toArray();
    initializeConversationProjects(storage);
    initializeConversationProjects(storage);
    expect(storage.sql.exec("SELECT * FROM conversation_projects ORDER BY agent_id").toArray()).toEqual(members);
    expect(storage.sql.exec("SELECT * FROM project_threads ORDER BY agent_id").toArray()).toEqual(links);
  }));

  it("lists canonical names and inherited grouping without changing task identity", () => inside(async (_, registry) => {
    const roster = await (await registry.fetch(new Request("https://user.internal/agents"))).json<Array<Record<string, unknown>>>();
    expect(roster.find(a => a.id === root)).toMatchObject({ title: "Project Alpha", projectName: "Project Alpha", projectRootId: root });
    expect(roster.find(a => a.id === child)).toMatchObject({ title: "Original title", projectName: "Project Alpha", projectRootId: root,
      parentAgentId: root, originTurnId: "request-1", projectTurnId: "project:task" });
    expect(roster.find(a => a.id === independent)).not.toHaveProperty("projectName");
  }));

  it("does not expose one-off membership writes through the account registry", () => inside(async (storage, registry) => {
    const before = storage.sql.exec("SELECT * FROM conversation_projects").toArray();
    for (const method of ["GET", "POST"]) {
      const response = await registry.fetch(new Request("https://user.internal/conversation-project-migration-20000101", { method }));
      expect(response.status).toBe(404);
    }
    expect(storage.sql.exec("SELECT * FROM conversation_projects").toArray()).toEqual(before);
  }));
});
