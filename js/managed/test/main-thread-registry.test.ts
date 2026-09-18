import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
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
    const deleted = await call("/main-thread");
    expect(deleted.status).toBe(410);
    expect(await deleted.json()).toEqual({ error: "main_thread_deleted" });
    expect((await call("/main-thread", "team-a", { agent_id: b })).status).toBe(409);
    expect(storage.sql.exec("SELECT agent_id FROM main_threads").toArray()).toEqual([{ agent_id: a }]);
    expect((await call("/main-thread", "team-a", { agent_id: a })).status).toBe(404);
  }));

  it("projects only scoped canonical roles without names or titles", () => inside(async storage => {
    const role = async (agent: string, team = "team-a") => (await mainThreadRegistry(request(`/canonical-role/${agent}`, team), storage)).json();
    expect(await role(a)).toEqual({ role: "conversation" });
    storage.sql.exec("INSERT INTO main_threads VALUES ('team-a',?)", a);
    expect(await role(a)).toEqual({ role: "main" });
    storage.sql.exec("DELETE FROM main_threads");
    storage.sql.exec("INSERT INTO canonical_projects VALUES ('team-a','build','Secret project name',?)", a);
    storage.sql.exec(`INSERT INTO project_threads(agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at)
      VALUES (?,?,?,'origin','turn','Secret title','hash',1)`, b, a, a);
    expect(await role(a)).toEqual({ role: "project_coordinator", project_id: "build", project_root_id: a });
    expect(await role(b)).toEqual({ role: "project_task", project_id: "build", project_root_id: a, parent_agent_id: a });
    for (const agent of [a, b, "missing"]) expect((await mainThreadRegistry(request(`/canonical-role/${agent}`, "team-b"), storage)).status).toBe(404);
    storage.sql.exec("DELETE FROM canonical_projects");
    expect(await role(a)).toEqual({ role: "conversation" });
    expect(await role(b)).toEqual({ role: "project_task", project_root_id: a, parent_agent_id: a });
    storage.sql.exec("INSERT INTO canonical_projects VALUES ('team-b','foreign','Hidden',?)", a);
    for (const agent of [a, b]) {
      const stale = await mainThreadRegistry(request(`/canonical-role/${agent}`), storage);
      expect(stale.status).toBe(404);
      expect(await stale.json()).toEqual({ error: "not_found" });
    }
    storage.sql.exec("DELETE FROM canonical_projects");
    storage.sql.exec("UPDATE agent_registry SET team_id='team-b' WHERE id=?", a);
    expect(await role(b)).toEqual({ role: "conversation" });
    storage.sql.exec("UPDATE agent_registry SET team_id='team-a',deleted_at=2 WHERE id=?", a);
    expect(await role(b)).toEqual({ role: "conversation" });
    expect((await mainThreadRegistry(request(`/canonical-role/${a}`), storage)).status).toBe(404);
    storage.sql.exec("DELETE FROM agent_registry WHERE id=?", a);
    expect(await role(b)).toEqual({ role: "conversation" });
  }));

  it("retains missing main tombstones and hides foreign stale metadata", () => inside(async storage => {
    storage.sql.exec("INSERT INTO main_threads VALUES ('team-a',?)", a);
    expect((await mainThreadRegistry(request(`/canonical-role/${b}`, "team-a", {}), storage)).status).toBe(405);
    storage.sql.exec("UPDATE agent_registry SET team_id='team-b' WHERE id=?", a);
    expect((await mainThreadRegistry(request(`/canonical-role/${a}`, "team-b"), storage)).status).toBe(404);
    const foreign = await mainThreadRegistry(request("/main-thread"), storage);
    expect(foreign.status).toBe(404);
    expect(await foreign.json()).toEqual({ error: "not_found" });
    storage.sql.exec("DELETE FROM agent_registry WHERE id=?", a);
    const missing = await mainThreadRegistry(request("/main-thread"), storage);
    expect(missing.status).toBe(410);
    expect(await missing.json()).toEqual({ error: "main_thread_deleted" });
    expect((await mainThreadRegistry(request("/main-thread", "team-a", { agent_id: b }), storage)).status).toBe(409);
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
    expect(await (await account.fetch(request(`/canonical-role/${a}`))).json()).toEqual({ role: "main" });
    expect((await other.fetch(request(`/canonical-role/${a}`))).status).toBe(404);
    expect((await other.fetch(request("/main-thread"))).status).toBe(404);
    expect((await other.fetch(request("/main-thread", "team-a", { agent_id: a }))).status).toBe(404);
    expect((await account.fetch(request("/projects/root", "team-a", { name: "Root", coordinator_agent_id: b }))).status).toBe(201);
    const preflight = (agent: string) => account.fetch(`https://user.internal/project-threads/${agent}?spawn_preflight=1`);
    const mainPreflight = await preflight(a);
    expect(mainPreflight.status).toBe(409);
    expect(await mainPreflight.json()).toEqual({ error: "main_thread_conflict" });
    expect((await preflight(b)).status).toBe(200);
    // Legacy unscoped registry rows remain eligible; preflight does not stamp a team.
    expect((await preflight(c)).status).toBe(200);
    await runInDurableObject(account, async (_, state) => {
      expect(state.storage.sql.exec<{ team_id: string | null }>("SELECT team_id FROM agent_registry WHERE id=?", c).toArray()[0]!.team_id).toBeNull();
      expect(state.storage.sql.exec("SELECT * FROM project_threads").toArray()).toEqual([]);
    });
    expect((await preflight(crypto.randomUUID())).status).toBe(404);
    expect((await other.fetch(`https://user.internal/project-threads/${b}?spawn_preflight=1`)).status).toBe(404);
    for (const [parent, child] of [[a, c], [c, a], [c, b]]) {
      expect((await account.fetch(`https://user.internal/project-threads/${parent}`, { method: "POST", body: JSON.stringify({
        agent_id: child, origin_turn_id: "origin", turn_id: "turn", title: "Child", request_hash: "a".repeat(64),
      }) })).status).toBe(409);
    }
    await runInDurableObject(sessions().getByName(b), async (_, state) => {
      state.storage.sql.exec("UPDATE session_state SET team_id='team-b'");
    });
    expect((await account.fetch(request("/projects/root", "team-a", { name: "Rename", coordinator_agent_id: b }))).status).toBe(404);
  });
  it("verifies a legacy coordinator through session RPC before stamping only registry team metadata", async () => {
    const coordinator = crypto.randomUUID(), child = crypto.randomUUID();
    const account = ns().getByName(crypto.randomUUID());
    await initializeAccountSessions(account, [coordinator]);
    const attach = (teamId?: string) => account.fetch("https://user.internal/agents", {
      method: "POST", body: JSON.stringify({ agentId: coordinator, teamId }),
    });
    expect((await attach()).status).toBe(204);
    await runInDurableObject(account, async (_, state) => {
      state.storage.sql.exec(`INSERT INTO project_threads(agent_id,parent_agent_id,project_root_id,origin_turn_id,turn_id,title,request_hash,created_at)
        VALUES (?,?,?,'origin','turn','Existing child','hash',1)`, child, coordinator, coordinator);
    });
    const registry = () => runInDurableObject(account, async (_, state) => ({
      agent: state.storage.sql.exec("SELECT * FROM agent_registry WHERE id=?", coordinator).toArray()[0]!,
      threads: state.storage.sql.exec("SELECT * FROM project_threads").toArray(),
    }));
    const identity = () => runInDurableObject(sessions().getByName(coordinator), async (_, state) =>
      state.storage.sql.exec("SELECT owner_id,team_id FROM session_state").toArray());
    const before = await registry();
    const sessionBefore = await identity();
    expect(before.agent.team_id).toBeNull();
    // Repeated unverified attaches must accept legacy rows without assigning a team.
    for (const team of ["team-a", "team-a", "team-b"]) expect((await attach(team)).status).toBe(204);
    expect(await registry()).toEqual(before);
    const project = { name: "Legacy", coordinator_agent_id: coordinator };
    expect((await account.fetch(request("/projects/legacy", "team-a", project))).status).toBe(201);
    expect((await account.fetch(request("/projects/legacy", "team-a", project))).status).toBe(200);
    expect(await registry()).toEqual({ ...before, agent: { ...before.agent, team_id: "team-a" } });
    expect(await identity()).toEqual(sessionBefore);
    expect(await (await account.fetch(request("/projects"))).json()).toEqual({
      data: [{ id: "legacy", ...project }],
    });
    expect((await attach("team-b")).status).toBe(409);
  });

  it("rejects legacy coordinators with foreign session owners or conflicting registry teams through session RPC", async () => {
    const foreign = crypto.randomUUID(), conflict = crypto.randomUUID();
    const account = ns().getByName(crypto.randomUUID());
    await initializeAccountSessions(account, [foreign, conflict]);
    await runInDurableObject(sessions().getByName(foreign), async (_, state) => {
      state.storage.sql.exec("UPDATE session_state SET owner_id=?", crypto.randomUUID());
    });
    for (const [agentId, teamId] of [[foreign, undefined], [conflict, "team-b"]]) {
      expect((await account.fetch("https://user.internal/agents", {
        method: "POST", body: JSON.stringify({ agentId, teamId }),
      })).status).toBe(204);
    }
    const snapshot = () => runInDurableObject(account, async (_, state) => ({
      agents: state.storage.sql.exec("SELECT * FROM agent_registry ORDER BY id").toArray(),
      projects: state.storage.sql.exec("SELECT * FROM canonical_projects").toArray(),
      threads: state.storage.sql.exec("SELECT * FROM project_threads").toArray(),
    }));
    const before = await snapshot();
    for (const agent of [foreign, conflict]) {
      expect((await account.fetch(request("/projects/rejected", "team-a", {
        name: "Rejected", coordinator_agent_id: agent,
      }))).status).toBe(404);
    }
    expect(await snapshot()).toEqual(before);
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
