import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { mainThreadRequest } from "../src/main-thread";

const users = () => (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace }).NANOCODEX_SESSIONS;

async function fixture() {
  const owner = crypto.randomUUID(), team = crypto.randomUUID();
  const account = users().getByName(owner);
  await runInDurableObject(account, async (_, state) => {
    await state.storage.put("account", { id: owner, organizationId: crypto.randomUUID(), persistent: true, createdAt: 1, lastAuthenticatedAt: 1 });
  });
  const creations = new Map<string, string>();
  const registry = (path: string, init?: RequestInit) => account.fetch(`https://user.internal${path}?team_id=${team}`, init);
  const host = { teamId: team, registry, create: async (key: string) => {
    let id = creations.get(key);
    if (!id) {
      id = crypto.randomUUID(); creations.set(key, id);
      await runInDurableObject(sessions().getByName(id), async (_, state) => {
        state.storage.sql.exec(`INSERT INTO session_state
          (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
          VALUES (1,?,?,'22222222-2222-4222-8222-222222222222',?,1,'https://nanocodex.example','managed',?)`, id!, owner, team, Date.now());
      });
      expect((await account.fetch("https://user.internal/agents", { method: "POST", body: JSON.stringify({ agentId: id, teamId: team }) })).status).toBe(204);
    }
    return Response.json({ agent_id: id });
  } };
  const ensure = (path: string) => mainThreadRequest(new Request(`https://nanocodex.example/v1${path}`, {
    method: "PUT", body: path === "/main-thread" ? "{}" : JSON.stringify({ name: "Research" }),
  }), host);
  const remove = (id: string) => account.fetch(`https://user.internal/agents/${id}`, { method: "DELETE" });
  return { account, creations, ensure, remove, registry, team };
}

describe("canonical conversation deletion and recreation", () => {
  it.each(["/main-thread", "/projects/research"])("recreates %s once after deletion without reviving the old identity", async path => {
    const f = await fixture();
    const readId = async (response: Response) => {
      expect(response.ok).toBe(true);
      const body = await response.json<{ agent_id?: string; coordinator_agent_id?: string }>();
      return (body.agent_id ?? body.coordinator_agent_id)!;
    };
    const first = await readId(await f.ensure(path));
    expect(await readId(await f.ensure(path))).toBe(first);
    expect(f.creations.size).toBe(1);
    expect((await f.remove(first)).status).toBe(204);
    expect((await f.remove(first)).status).toBe(204);
    const second = await readId(await f.ensure(path));
    expect(second).not.toBe(first);
    expect(await readId(await f.ensure(path))).toBe(second);
    expect(f.creations.size).toBe(2);
    const base = `canonical:${f.team}:${path === "/main-thread" ? "main" : "project:research"}`;
    expect([...f.creations.keys()]).toEqual([base, `${base}:generation:1`]);
    const body = path === "/main-thread" ? { agent_id: first } : { name: "Research", coordinator_agent_id: first };
    expect((await f.registry(path, { method: "PUT", body: JSON.stringify(body) })).status).toBe(404);
    const unrelated = await f.account.fetch(`https://user.internal/canonical-generations/main?team_id=${crypto.randomUUID()}`);
    expect(await unrelated.json()).toEqual({ generation: 0 });
  });

  it.each(["scoped", "legacy"])("recreates a %s projected identity only after scoped retirement", async kind => {
    const f = await fixture();
    const first = await (await f.ensure("/projects/original")).json<{ coordinator_agent_id: string }>();
    const root = first.coordinator_agent_id;
    const path = `/projects/project-${root}`;
    const member = crypto.randomUUID();
    await runInDurableObject(f.account, async (_, state) => {
      state.storage.sql.exec("DELETE FROM canonical_projects WHERE coordinator_agent_id=?", root);
      state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", root, root, "Legacy");
      state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", member, root, "Legacy");
      if (kind === "legacy") state.storage.sql.exec("UPDATE agent_registry SET team_id=NULL WHERE id=?", root);
    });
    const replacement = await (await f.ensure("/projects/replacement")).json<{ coordinator_agent_id: string }>();
    await runInDurableObject(f.account, async (_, state) => {
      state.storage.sql.exec("DELETE FROM canonical_projects WHERE coordinator_agent_id=?", replacement.coordinator_agent_id);
    });
    const claim = () => f.registry(path, { method: "PUT", body: JSON.stringify({ name: "Hijack", coordinator_agent_id: replacement.coordinator_agent_id }) });
    expect((await claim()).status).toBe(409);
    if (kind === "legacy") expect((await f.remove(root)).status).toBe(409);
    expect((await f.registry(`/agents/${root}`, { method: "DELETE" })).status).toBe(204);
    expect((await f.registry(`/agents/${root}`, { method: "DELETE" })).status).toBe(204);
    expect(await (await f.registry(`/canonical-generations/projects/project-${root}`)).json()).toEqual({ generation: 1 });
    const recreated = await f.ensure(path);
    expect(recreated.status).toBe(201);
    const newRoot = (await recreated.json<{ coordinator_agent_id: string }>()).coordinator_agent_id;
    expect(newRoot).not.toBe(root);
    expect([...f.creations.keys()]).toContain(`canonical:${f.team}:project:project-${root}:generation:1`);
    expect((await f.registry(path, { method: "PUT", body: JSON.stringify({ name: "Revive", coordinator_agent_id: root }) })).status).toBe(404);
    expect((await f.remove(newRoot)).status).toBe(204);
    expect(await (await f.registry(`/canonical-generations/projects/project-${root}`)).json()).toEqual({ generation: 2 });
    expect((await f.registry(`/agents/${root}`, { method: "DELETE" })).status).toBe(204);
    expect(await (await f.registry(`/canonical-generations/projects/project-${root}`)).json()).toEqual({ generation: 2 });
    const third = (await (await f.ensure(path)).json<{ coordinator_agent_id: string }>()).coordinator_agent_id;
    expect(third).not.toBe(root);
    expect(third).not.toBe(newRoot);
    await runInDurableObject(f.account, async (_, state) => {
      const rows = state.storage.sql.exec("SELECT * FROM conversation_projects").toArray();
      expect(rows).toHaveLength(2);
      expect(rows).toEqual(expect.arrayContaining([
        { agent_id: root, project_root_id: root, project_name: "Legacy" },
        { agent_id: member, project_root_id: root, project_name: "Legacy" },
      ]));
    });
  });

  it("retires remaining canonical references on an already tombstoned root", async () => {
    const f = await fixture();
    const first = (await (await f.ensure("/main-thread")).json<{ agent_id: string }>()).agent_id;
    await runInDurableObject(f.account, async (_, state) => {
      state.storage.sql.exec("UPDATE agent_registry SET deleted_at=1 WHERE id=?", first);
    });
    expect((await f.remove(first)).status).toBe(204);
    expect((await f.remove(first)).status).toBe(204);
    expect(await (await f.registry("/canonical-generations/main")).json()).toEqual({ generation: 1 });
    const next = (await (await f.ensure("/main-thread")).json<{ agent_id: string }>()).agent_id;
    expect(next).not.toBe(first);
  });

  it("leaves generations untouched when ordinary conversations are deleted", async () => {
    const f = await fixture();
    expect((await f.remove(crypto.randomUUID())).status).toBe(204);
    expect(await (await f.registry("/canonical-generations/main")).json()).toEqual({ generation: 0 });
    expect(await (await f.registry("/canonical-generations/projects/research")).json()).toEqual({ generation: 0 });
  });
});
