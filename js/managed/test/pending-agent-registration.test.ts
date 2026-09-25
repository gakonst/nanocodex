import { env, evictDurableObject, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import {
  attachAgent, detachAgent, listAgents, prepareAgentRegistration, publishAgentRegistration,
  type AccountAuthEnv, type UserAccount,
} from "../src/account-auth";

const runtime = env as AccountAuthEnv;
const account = (owner: string) => runtime.NANOCODEX_USERS.getByName(owner) as DurableObjectStub<UserAccount>;
const countPending = async (owner: string, id: string) => runInDurableObject(account(owner), (_account, state) =>
  state.storage.sql.exec<{ count: number }>(
    "SELECT COUNT(*) AS count FROM agent_registry_pending WHERE id = ?", id,
  ).one().count);

const post = (owner: string, id: string, phase: "prepare" | "publish") =>
  account(owner).fetch(`https://user.internal/agents/${id}/${phase}`, {
    method: "POST", headers: { "content-type": "application/json" }, body: phase === "publish" ? "{}" : undefined,
  });

describe("invisible account registration", () => {
  it("holds a speculative preparation out of discovery across a restart, and publishes exactly once", async () => {
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    await prepareAgentRegistration(runtime, owner, id);
    expect(await countPending(owner, id)).toBe(1);
    expect(await listAgents(runtime, owner)).toEqual([]);
    await evictDurableObject(account(owner));
    expect(await listAgents(runtime, owner)).toEqual([]);
    await prepareAgentRegistration(runtime, owner, id); // retry after uncertain response
    await publishAgentRegistration(runtime, owner, id, undefined, false);
    await publishAgentRegistration(runtime, owner, id, undefined, true);
    expect(await countPending(owner, id)).toBe(0);
    expect(await listAgents(runtime, owner)).toMatchObject([{ id, mayHaveScheduledJobs: true }]);
    await prepareAgentRegistration(runtime, owner, id); // late hint cannot regress publication
    expect(await countPending(owner, id)).toBe(0);
    expect((await listAgents(runtime, owner)).map(agent => agent.id)).toEqual([id]);
  });

  it("publishes if the hint was lost or reordered, and cannot publish after a durable deletion", async () => {
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    await publishAgentRegistration(runtime, owner, id);
    await prepareAgentRegistration(runtime, owner, id);
    expect(await listAgents(runtime, owner)).toHaveLength(1);
    await detachAgent(runtime, owner, id);
    await evictDurableObject(account(owner));
    expect(await listAgents(runtime, owner)).toEqual([]);
    expect((await post(owner, id, "publish")).status).toBe(410);
    expect((await post(owner, id, "prepare")).status).toBe(410);
    expect(await countPending(owner, id)).toBe(0);
    // Existing live/legacy publication must respect the same tombstone.
    await expect(attachAgent(runtime, owner, id)).rejects.toThrow();
  });

  it("fences both permutations of an in-flight prepare/delete race", async () => {
    for (const prepareFirst of [true, false]) {
      const owner = crypto.randomUUID(), id = crypto.randomUUID();
      if (prepareFirst) await prepareAgentRegistration(runtime, owner, id);
      await detachAgent(runtime, owner, id);
      if (!prepareFirst) expect((await post(owner, id, "prepare")).status).toBe(410);
      expect((await post(owner, id, "publish")).status).toBe(410);
      expect(await listAgents(runtime, owner)).toEqual([]);
      expect(await countPending(owner, id)).toBe(0);
    }
  });

  it("expires abandoned hints by account alarm without exposing them", async () => {
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    await prepareAgentRegistration(runtime, owner, id);
    await runInDurableObject(account(owner), async (user, state) => {
      state.storage.sql.exec("UPDATE agent_registry_pending SET expires_at = ? WHERE id = ?", Date.now() - 1, id);
      await user.alarm();
    });
    expect(await countPending(owner, id)).toBe(0);
    expect(await listAgents(runtime, owner)).toEqual([]);
  });
});

describe("mixed-version account rollout", () => {
  it("falls back to legacy attachment only when the old account DO lacks /publish", async () => {
    const seen: string[] = [];
    const legacy = { NANOCODEX_USERS: { getByName: () => ({ fetch: async (request: Request | string) => {
      const url = new URL(typeof request === "string" ? request : request.url).pathname;
      seen.push(url);
      return new Response(null, { status: url.endsWith("/publish") ? 404 : 204 });
    } }) } } as unknown as AccountAuthEnv;
    await publishAgentRegistration(legacy, crypto.randomUUID(), crypto.randomUUID());
    expect(seen).toEqual([expect.stringMatching(/\/publish$/), "/agents"]);
    const rejected: string[] = [];
    const tombstone = { NANOCODEX_USERS: { getByName: () => ({ fetch: async (request: Request | string) => {
      rejected.push(new URL(typeof request === "string" ? request : request.url).pathname);
      return new Response(null, { status: 410 });
    } }) } } as unknown as AccountAuthEnv;
    await expect(publishAgentRegistration(tombstone, crypto.randomUUID(), crypto.randomUUID())).rejects.toThrow();
    expect(rejected).toHaveLength(1);
  });
});
