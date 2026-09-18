import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { mainThreadRequest } from "../src/main-thread";
import { initializeMainThreadRegistry, mainThreadRegistry } from "../src/main-thread-registry";

const a = "11111111-1111-4111-8111-111111111111";
const b = "22222222-2222-4222-8222-222222222222";
const c = "33333333-3333-4333-8333-333333333333";
const request = (path: string, team = "team-a", body?: unknown) => new Request(`https://user.internal${path}?team_id=${team}`, {
  method: body === undefined ? "GET" : "PUT", ...(body === undefined ? {} : { body: JSON.stringify(body) }),
});
const ns = () => (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace }).NANOCODEX_SESSIONS;
async function initializeAccountSessions(account: DurableObjectStub, agentIds: string[]) {
  const owner = crypto.randomUUID();
  await runInDurableObject(account, async (_, state) => {
    await state.storage.put("account", { id: owner, organizationId: crypto.randomUUID(), persistent: true, createdAt: 1, lastAuthenticatedAt: 1 });
  });
  for (const agent of agentIds) await runInDurableObject(sessions().getByName(agent), async (_, state) => {
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES (1,?,?,'22222222-2222-4222-8222-222222222222','team-a',1,'https://nanocodex.example','managed',?)`, agent, owner, Date.now());
  });
}
async function inside(test: (storage: DurableObjectStorage) => Promise<void>) {
  await runInDurableObject(ns().getByName(crypto.randomUUID()), async (_, state) => {
    initializeMainThreadRegistry(state.storage);
    for (const [agent, team] of [[a, "team-a"], [b, "team-a"], [c, "team-b"]])
      state.storage.sql.exec("INSERT INTO agent_registry(id,created_at,updated_at,team_id) VALUES (?,1,1,?)", agent!, team!);
    await test(state.storage);
  });
}

describe("account main and canonical project registry", () => {
  it("discovers migrated roots without writes and reuses their stable coordinator on PUT", () => inside(async storage => {
    storage.sql.exec("UPDATE agent_registry SET team_id=NULL WHERE id=?", a);
    storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", a, a, "Migrated");
    storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", b, a, "Migrated");
    const snapshot = () => ["agent_registry", "conversation_projects", "project_threads", "canonical_projects", "main_threads"]
      .map(table => storage.sql.exec(`SELECT * FROM ${table}`).toArray());
    const before = snapshot();
    const host = { teamId: "team-a", create: async () => { throw new Error("must reuse migrated coordinator"); },
      registry: (path: string, init?: RequestInit) => mainThreadRegistry(new Request(`https://user.internal${path}?team_id=team-a`, init), storage,
        async (agent, team) => agent === a && team === "team-a") };
    expect(await (await mainThreadRequest(new Request("https://x/v1/projects"), host)).json()).toEqual({ data: [
      { id: `project-${a}`, name: "Migrated", coordinator_agent_id: a },
    ] });
    expect(snapshot()).toEqual(before);
    expect(await (await mainThreadRegistry(request("/projects"), storage)).json()).toEqual({ data: [] });
    const put = (coordinator?: string) => mainThreadRequest(new Request(`https://x/v1/projects/project-${a}`, {
      method: "PUT", body: JSON.stringify({ name: "Renamed", ...(coordinator ? { coordinator_agent_id: coordinator } : {}) }),
    }), host);
    expect((await put(b)).status).toBe(409);
    expect((await mainThreadRegistry(request(`/projects/project-${a}`, "team-a", { name: "Hijack", coordinator_agent_id: b }), storage)).status).toBe(409);
    expect((await put()).status).toBe(201);
    expect(storage.sql.exec("SELECT * FROM conversation_projects").toArray()).toEqual(before[1]);
    expect(storage.sql.exec("SELECT * FROM project_threads").toArray()).toEqual(before[2]);
    expect(await (await host.registry("/projects")).json()).toEqual({ data: [
      { id: `project-${a}`, name: "Renamed", coordinator_agent_id: a },
    ] });
  }));

  it("filters foreign, deleted, Main and execution-child roots and rechecks after validation", () => inside(async storage => {
    for (const agent of [a, b, c]) storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", agent, agent, "Migrated");
    const list = (validate?: (agent: string, team: string) => Promise<boolean>) => mainThreadRegistry(request("/projects"), storage, validate);
    expect(await (await list(async () => false)).json()).toEqual({ data: [] });
    expect((await list(async () => { throw new Error("unavailable"); })).status).toBe(503);
    storage.sql.exec("INSERT INTO main_threads VALUES ('team-a',?)", a);
    storage.sql.exec(`INSERT INTO project_threads VALUES (?,?,?,'origin','turn','child','hash',1)`, b, a, a);
    expect(await (await list(async () => true)).json()).toEqual({ data: [] });
    storage.sql.exec("DELETE FROM main_threads");
    expect(await (await list(async () => {
      storage.sql.exec("UPDATE agent_registry SET deleted_at=2 WHERE id=?", a);
      return true;
    })).json()).toEqual({ data: [] });
  }));

  it("gives canonical metadata precedence and deduplicates both coordinator and project ID", () => inside(async storage => {
    for (const agent of [a, b]) storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", agent, agent, "Migrated");
    storage.sql.exec("INSERT INTO canonical_projects VALUES ('team-a','custom','Canonical',?)", a);
    // A pre-existing canonical ID also wins over a migrated root with that synthetic ID.
    storage.sql.exec("INSERT INTO canonical_projects VALUES ('team-a',?,'ID winner',?)", `project-${b}`, b);
    expect(await (await mainThreadRegistry(request("/projects"), storage, async () => true)).json()).toEqual({ data: [
      { id: "custom", name: "Canonical", coordinator_agent_id: a },
      { id: `project-${b}`, name: "ID winner", coordinator_agent_id: b },
    ] });
    storage.sql.exec("DELETE FROM canonical_projects");
    storage.sql.exec("INSERT INTO canonical_projects VALUES ('team-a',?,'Collision',?)", `project-${a}`, b);
    expect(await (await mainThreadRegistry(request("/projects"), storage, async () => true)).json()).toEqual({ data: [
      { id: `project-${a}`, name: "Collision", coordinator_agent_id: b },
    ] });
  }));

  it("isolates teams, requires existing live owned agents, and retains main identity", () => inside(async storage => {
    const call = (path: string, team = "team-a", body?: unknown) => mainThreadRegistry(request(path, team, body), storage);
    expect((await call("/main-thread")).status).toBe(404);
    expect((await call("/main-thread", "team-a", { agent_id: c })).status).toBe(404);
    expect((await call("/main-thread", "team-a", { agent_id: "foreign" })).status).toBe(404);
    expect((await call("/main-thread", "team-a", { agent_id: a })).status).toBe(201);
    expect((await call("/main-thread", "team-a", { agent_id: a })).status).toBe(200);
    expect((await call("/main-thread", "team-a", { agent_id: b })).status).toBe(409);
    expect(await (await call("/main-thread")).json()).toEqual({ agent_id: a });
    expect((await call("/main-thread", "team-b")).status).toBe(404);
    storage.sql.exec("UPDATE agent_registry SET deleted_at=2 WHERE id=?", a);
    expect((await call("/main-thread")).status).toBe(404);
    expect((await call("/main-thread", "team-a", { agent_id: a })).status).toBe(404);
  }));

  it("keeps projects distinct from main, allows rename and rejects coordinator reassignment", () => inside(async storage => {
    const call = (path: string, team = "team-a", body?: unknown) => mainThreadRegistry(request(path, team, body), storage);
    const project = { name: "Build", coordinator_agent_id: b };
    expect((await call("/main-thread", "team-a", { agent_id: a })).status).toBe(201);
    expect((await call("/projects/main", "team-a", { ...project, coordinator_agent_id: a })).status).toBe(409);
    expect((await call("/projects/build_1", "team-a", project)).status).toBe(201);
    expect((await call("/projects/build_1", "team-a", { ...project, name: "Renamed" })).status).toBe(200);
    expect((await call("/projects/other", "team-a", project)).status).toBe(409);
    expect((await call("/projects/build_1", "team-a", { ...project, coordinator_agent_id: a })).status).toBe(409);
    expect((await call("/projects/build_1", "team-b", { ...project, coordinator_agent_id: c })).status).toBe(201);
    expect(await (await call("/projects")).json()).toEqual({ data: [{ id: "build_1", name: "Renamed", coordinator_agent_id: b }] });
    storage.sql.exec("UPDATE agent_registry SET deleted_at=2 WHERE id=?", b);
    expect(await (await call("/projects")).json()).toEqual({ data: [] });
    expect((await call("/projects/build_1", "team-a", project)).status).toBe(404);
  }));

  it("rejects project children as main/coordinators and existing roots as main", () => inside(async storage => {
    storage.sql.exec(`INSERT INTO project_threads(agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at)
      VALUES (?,?,?,'origin','turn','child','hash',1)`, b, a, a);
    for (const agent of [a, b]) expect((await mainThreadRegistry(request("/main-thread", "team-a", { agent_id: agent }), storage)).status).toBe(409);
    expect((await mainThreadRegistry(request("/projects/child", "team-a", { name: "Child", coordinator_agent_id: b }), storage)).status).toBe(409);
    expect((await mainThreadRegistry(request("/projects/root", "team-a", { name: "Root", coordinator_agent_id: a }), storage)).status).toBe(201);
  }));

  it("validates route ids, scope and strict input", () => inside(async storage => {
    for (const req of [request("/main-thread", "", { agent_id: a }), request("/projects/bad.id", "team-a", { name: "Valid", coordinator_agent_id: a }),
      request("/projects/valid", "team-a", { name: " ", coordinator_agent_id: a }), request("/main-thread", "team-a", { agent_id: a, team_id: "team-b" }),
      new Request("https://user.internal/main-thread?team_id=team-a", { method: "PUT", body: "{" })])
      expect((await mainThreadRegistry(req, storage)).status).toBe(400);
  }));

  it("hooks account routes, isolates accounts, preserves team registration and prevents later reparenting", async () => {
    const a = crypto.randomUUID(), b = crypto.randomUUID(), c = crypto.randomUUID();
    const account = ns().getByName(crypto.randomUUID());
    await initializeAccountSessions(account, [a, b]);
    const other = ns().getByName(crypto.randomUUID());
    const register = (agentId: string, teamId?: string) => account.fetch("https://user.internal/agents", { method: "POST", body: JSON.stringify({ agentId, teamId }) });
    expect((await register(a, "team-a")).status).toBe(204);
    expect((await register(a, "team-b")).status).toBe(409);
    expect((await register(b, "team-a")).status).toBe(204);
    expect((await register(c)).status).toBe(204);
    expect((await register(c, "team-a")).status).toBe(204);
    expect((await account.fetch(request("/projects/unverified", "team-a", { name: "Unverified", coordinator_agent_id: c }))).status).toBe(404);
    expect((await account.fetch(request("/main-thread", "team-a", { agent_id: a }))).status).toBe(201);
    expect((await other.fetch(request("/main-thread"))).status).toBe(404);
    expect((await other.fetch(request("/main-thread", "team-a", { agent_id: a }))).status).toBe(404);
    expect((await account.fetch(request("/projects/root", "team-a", { name: "Root", coordinator_agent_id: b }))).status).toBe(201);
    for (const [parent, child] of [[a, c], [c, a], [c, b]]) {
      expect((await account.fetch(`https://user.internal/project-threads/${parent}`, { method: "POST", body: JSON.stringify({
        agent_id: child, origin_turn_id: "origin", turn_id: "turn", title: "Child", request_hash: "a".repeat(64),
      }) })).status).toBe(409);
    }
    await runInDurableObject(account, async (_, state) => {
      state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", b, b, "Migrated");
    });
    expect(await (await account.fetch(request("/projects"))).json()).toEqual({ data: [{ id: "root", name: "Root", coordinator_agent_id: b }] });
    await runInDurableObject(sessions().getByName(b), async (_, state) => {
      state.storage.sql.exec("UPDATE session_state SET owner_id=?", crypto.randomUUID());
    });
    expect(await (await account.fetch(request("/projects"))).json()).toEqual({ data: [] });
    await runInDurableObject(sessions().getByName(b), async (_, state) => {
      state.storage.sql.exec("UPDATE session_state SET team_id='team-b'");
    });
    expect((await account.fetch(request("/projects/root", "team-a", { name: "Rename", coordinator_agent_id: b }))).status).toBe(404);
  });
  it("serializes competing main registration and project membership", async () => {
    const a = crypto.randomUUID(), b = crypto.randomUUID();
    const account = ns().getByName(crypto.randomUUID());
    await initializeAccountSessions(account, [a, b]);
    for (const agentId of [a, b]) await account.fetch("https://user.internal/agents", {
      method: "POST", body: JSON.stringify({ agentId, teamId: "team-a" }),
    });
    const responses = await Promise.all([
      account.fetch(request("/main-thread", "team-a", { agent_id: b })),
      account.fetch(`https://user.internal/project-threads/${a}`, { method: "POST", body: JSON.stringify({
        agent_id: b, origin_turn_id: "origin", turn_id: "turn", title: "Child", request_hash: "a".repeat(64),
      }) }),
    ]);
    expect(responses.map(response => response.status).sort()).toEqual([201, 409]);
  });

  it("requires session identity approval and rechecks local state after asynchronous validation", () => inside(async storage => {
    const req = () => request("/main-thread", "team-a", { agent_id: a });
    expect((await mainThreadRegistry(req(), storage, async (agent, team) => {
      expect([agent, team]).toEqual([a, "team-a"]);
      return false;
    })).status).toBe(404);
    expect((await mainThreadRegistry(req(), storage, async () => { throw new Error("Unavailable"); })).status).toBe(503);
    expect((await mainThreadRegistry(req(), storage, async () => {
      await Promise.resolve();
      storage.sql.exec("UPDATE agent_registry SET deleted_at=2 WHERE id=?", a);
      return true;
    })).status).toBe(404);
    expect(storage.sql.exec("SELECT * FROM main_threads").toArray()).toEqual([]);
    expect((await mainThreadRegistry(request("/projects/valid", "team-a", { name: "Valid", coordinator_agent_id: b }), storage, async () => true)).status).toBe(201);
  }));

  it("adopts a legacy coordinator only with verified session ownership and team", () => inside(async storage => {
    storage.sql.exec("UPDATE agent_registry SET team_id=NULL WHERE id=?", a);
    const req = () => request("/projects/legacy", "team-a", { name: "Legacy", coordinator_agent_id: a });
    expect((await mainThreadRegistry(req(), storage)).status).toBe(404);
    expect((await mainThreadRegistry(req(), storage, async () => false)).status).toBe(404);
    expect(storage.sql.exec<{ team_id: string | null }>("SELECT team_id FROM agent_registry WHERE id=?", a).toArray()[0]!.team_id).toBeNull();
    expect((await mainThreadRegistry(req(), storage, async (agent, team) => agent === a && team === "team-a")).status).toBe(201);
    expect(storage.sql.exec<{ team_id: string }>("SELECT team_id FROM agent_registry WHERE id=?", a).toArray()[0]!.team_id).toBe("team-a");
    expect((await mainThreadRegistry(request("/projects/other", "team-b", { name: "Other", coordinator_agent_id: a }), storage, async () => true)).status).toBe(404);
    expect(await (await mainThreadRegistry(request("/projects"), storage)).json()).toEqual({ data: [{ id: "legacy", name: "Legacy", coordinator_agent_id: a }] });
  }));

});
