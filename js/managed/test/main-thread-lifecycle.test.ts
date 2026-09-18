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

  it("leaves generations untouched when ordinary conversations are deleted", async () => {
    const f = await fixture();
    expect((await f.remove(crypto.randomUUID())).status).toBe(204);
    expect(await (await f.registry("/canonical-generations/main")).json()).toEqual({ generation: 0 });
    expect(await (await f.registry("/canonical-generations/projects/research")).json()).toEqual({ generation: 0 });
  });
});
