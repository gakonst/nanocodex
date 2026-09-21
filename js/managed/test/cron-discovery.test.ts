import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { attachAgent, detachAgent, listAgents, recordAgentCronPresence, UserAccount, type Principal } from "../src/account-auth";
import { CronTriggers, parseCronTrigger } from "../src/cron-triggers";

const runtime = env as Parameters<typeof worker.fetch>[1];
const config = { cron: "0 9 * * *", input: "Read-only discovery fixture", enabled: false };
const principal = (): Principal => {
  const userId = crypto.randomUUID();
  return { kind: "api_key", userId, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: `user:${userId}`, credentialId: "test", authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"] };
};

async function initialize(id: string, owner: Principal, knownEmpty?: boolean) {
  await attachAgent(runtime, owner.userId, id, undefined, knownEmpty === undefined ? undefined : !knownEmpty);
  await runInDurableObject(runtime.NANOCODEX_SESSIONS.getByName(id), async (_session, state) => {
    state.storage.sql.exec(`INSERT INTO session_state (
      singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active
    ) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`,
    id, owner.userId, owner.organizationId, owner.teamId, Date.now());
  });
}

describe("account cron discovery on real Durable Objects", () => {
  it("reduces a 100-agent account from 101 public reads to 3 after legacy backfill", async () => {
    const owner = principal();
    const emptyIDs = Array.from({ length: 97 }, () => crypto.randomUUID());
    for (const id of emptyIDs) await attachAgent(runtime, owner.userId, id, undefined, false);
    const legacyEmpty = crypto.randomUUID(), legacyCron = crypto.randomUUID(), newCron = crypto.randomUUID();
    await initialize(legacyEmpty, owner);
    await initialize(legacyCron, owner);
    await initialize(newCron, owner, true);
    // An existing schedule predates the account index and must remain discoverable.
    await runInDurableObject(runtime.NANOCODEX_SESSIONS.getByName(legacyCron), async (_session, state) => {
      new CronTriggers(state.storage).put("legacy", parseCronTrigger(config, Date.now()), '{"capabilities":[]}', 1, "hash", Date.now());
    });
    const reads: string[] = [];
    const observed = { ...runtime, NANOCODEX_SESSIONS: {
      getByName(id: string) { reads.push(id); return runtime.NANOCODEX_SESSIONS.getByName(id); },
    } } as typeof runtime;
    const call = (path: string, method = "GET", actor = owner) => worker.fetch(new Request(`https://nanocodex.example${path}`, {
      method, ...(method === "PUT" ? { body: JSON.stringify(config) } : {}),
    }), observed, createExecutionContext(), actor);
    expect((await call(`/v1/agents/${newCron}/triggers/new`, "PUT")).status).toBe(201);

    async function discover() {
      reads.length = 0;
      const response = await call("/v1/agents");
      expect(response.status).toBe(200);
      const body = await response.json<{ data: string[]; summaries: Record<string, { may_have_scheduled_jobs: boolean }> }>();
      expect(body.data).toHaveLength(100);
      expect(reads, "The account listing must not wake individual agents").toEqual([]);
      const jobs: { owner: string; id: string }[] = [];
      for (const id of body.data.filter((id) => body.summaries[id]!.may_have_scheduled_jobs !== false)) {
        const response = await call(`/v1/agents/${id}/triggers`);
        expect(response.status).toBe(200);
        const payload = await response.json<{ data: { id: string }[] }>();
        expect(JSON.stringify(payload)).not.toMatch(/authorization|revision|request_hash/);
        jobs.push(...payload.data.map((job) => ({ owner: id, id: job.id })));
      }
      return jobs;
    }
    expect(await discover()).toHaveLength(2);
    expect(reads.sort()).toEqual([legacyEmpty, legacyCron, newCron].sort());
    expect(await discover()).toHaveLength(2);
    expect(reads.sort()).toEqual([legacyCron, newCron].sort());
    expect(1 + reads.length).toBe(3); // Previously: listing + all 100 trigger lists.
    expect(reads.some((id) => emptyIDs.includes(id))).toBe(false);

    const grant: Principal = { ...owner, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } };
    const grantList = await (await call("/v1/agents", "GET", grant)).json<{ summaries: Record<string, object> }>();
    expect(Object.values(grantList.summaries).every((summary) => !("may_have_scheduled_jobs" in summary))).toBe(true);
    expect((await call(`/v1/agents/${newCron}/triggers`, "GET", grant)).status).toBe(403);
    expect((await call("/v1/agents", "GET", { ...owner, capabilities: [] })).status).toBe(403);
    const other = principal();
    expect(await (await call("/v1/agents", "GET", other)).json()).toEqual({ data: [], summaries: {} });
    expect((await call(`/v1/agents/${newCron}/triggers`, "GET", other)).status).toBe(404);
  });

  it("never lets a late empty backfill or attachment replay hide a saved schedule", async () => {
    const owner = principal(), id = crypto.randomUUID();
    await attachAgent(runtime, owner.userId, id);
    expect((await listAgents(runtime, owner.userId))[0]!.mayHaveScheduledJobs).toBe(true);
    await recordAgentCronPresence(runtime, owner.userId, id, false);
    expect((await listAgents(runtime, owner.userId))[0]!.mayHaveScheduledJobs).toBe(false);
    await attachAgent(runtime, owner.userId, id, undefined, true);
    expect((await listAgents(runtime, owner.userId))[0]!.mayHaveScheduledJobs).toBe(true);
    await recordAgentCronPresence(runtime, owner.userId, id, true);
    await recordAgentCronPresence(runtime, owner.userId, id, false);
    await attachAgent(runtime, owner.userId, id, undefined, false);
    expect((await listAgents(runtime, owner.userId))[0]!.mayHaveScheduledJobs).toBe(true);
    await expect(recordAgentCronPresence(runtime, principal().userId, id, false)).rejects.toThrow();
    await detachAgent(runtime, owner.userId, id);
    await expect(recordAgentCronPresence(runtime, owner.userId, id, true)).rejects.toThrow();
    expect(await listAgents(runtime, owner.userId)).toEqual([]);
  });

  it("does not commit a schedule if publishing its presence fails, but still serves reads", async () => {
    const owner = principal(), id = crypto.randomUUID();
    await initialize(id, owner, true);
    await runInDurableObject(runtime.NANOCODEX_SESSIONS.getByName(id), async (session, state) => {
      Object.defineProperty(session, "env", { value: { ...runtime,
        NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 503 }) }) },
      } });
      const saved = await session.fetch(new Request("https://session.internal/triggers/new", { method: "PUT", body: JSON.stringify(config) }));
      expect(saved.status).toBe(500);
      expect(new CronTriggers(state.storage).list()).toEqual([]);
      const read = await session.fetch(new Request("https://session.internal/triggers"));
      expect(read.status).toBe(200);
      expect(await read.json()).toEqual({ data: [] });
      expect(await state.storage.getAlarm()).toBeNull();
    });
  });

  it("migrates the old registry without mistaking existing agents for empty ones", async () => {
    const owner = principal(), id = crypto.randomUUID();
    const account = runtime.NANOCODEX_USERS.getByName(owner.userId);
    await runInDurableObject(account, async (_account, state) => {
      state.storage.sql.exec("ALTER TABLE agent_registry DROP COLUMN cron_candidate");
      state.storage.sql.exec("INSERT INTO agent_registry (id, title, created_at, updated_at, turn_count) VALUES (?, 'Existing agent', 1, 1, 2)", id);
      const restored = new UserAccount(state, runtime);
      const response = await restored.fetch(new Request("https://user.internal/agents"));
      expect(await response.json()).toEqual([{ id, title: "Existing agent", createdAt: 1, updatedAt: 1, turnCount: 2, mayHaveScheduledJobs: true }]);
    });
  });
});

describe("partial schedule updates", () => {
  it("preserves omitted fields, rejects unknown schedules and denies Connect or insufficient capabilities", async () => {
    const owner = principal(), id = crypto.randomUUID();
    await initialize(id, owner, true);
    const call = (method: string, body?: object, actor = owner, trigger = "morning") => worker.fetch(new Request(
      `https://nanocodex.example/v1/agents/${id}/triggers/${trigger}`, {
        method, ...(body ? { body: JSON.stringify(body) } : {}),
      }), runtime, createExecutionContext(), actor);
    expect((await call("PUT", { ...config, timezone: "Europe/Athens", session_mode: "continue", enabled: true })).status).toBe(201);
    const paused = await call("PATCH", { enabled: false });
    expect(paused.status).toBe(200);
    expect(await paused.json()).toMatchObject({ ...config, timezone: "Europe/Athens", session_mode: "continue", next_run_at: null });
    const resumed = await call("PATCH", { enabled: true });
    expect(resumed.status).toBe(200);
    expect(await resumed.json()).toMatchObject({ enabled: true, input: config.input, timezone: "Europe/Athens" });
    expect((await call("PATCH", { enabled: false }, owner, "missing")).status).toBe(404);
    expect((await call("PATCH", { authorization: {} })).status).toBe(400);
    expect((await call("PATCH", { enabled: false }, { ...owner, capabilities: ["agents:write"] })).status).toBe(403);
    expect((await call("PATCH", { enabled: false }, { ...owner, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } })).status).toBe(403);
    expect((await call("DELETE")).status).toBe(204);
    expect((await call("GET")).status).toBe(404);
  });
});

describe("cross-agent schedule authorization", () => {
  it("limits reads, updates and deletes to the caller account and team", async () => {
    const owner = principal(), source = crypto.randomUUID(), target = crypto.randomUUID();
    await initialize(source, owner, true);
    await initialize(target, owner, true);
    const call = (method: string, actor = owner, body?: object) => worker.fetch(new Request(
      `https://nanocodex.example/v1/agents/${target}/triggers${method === "GET" ? "" : "/morning"}`, {
        method, ...(body ? { body: JSON.stringify(body) } : {}),
      }), runtime, createExecutionContext(), actor);
    expect((await call("PUT", owner, config)).status).toBe(201);
    const otherUser = principal();
    const otherTeam = { ...owner, teamId: crypto.randomUUID() };
    const grant: Principal = { ...owner, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } };
    for (const method of ["GET", "PATCH", "DELETE"]) {
      const body = method === "PATCH" ? { enabled: true } : undefined;
      expect((await call(method, otherUser, body)).status).toBe(404);
      expect((await call(method, otherTeam, body)).status).toBe(404);
      expect((await call(method, grant, body)).status).toBe(403);
      expect((await call(method, { ...owner, capabilities: ["tools:use"] }, body)).status).toBe(403);
    }
    const listed = await call("GET");
    expect(listed.status).toBe(200);
    const payload = await listed.json<{ data: object[] }>();
    expect(payload.data).toHaveLength(1);
    expect(JSON.stringify(payload)).not.toMatch(/authorization|revision|request_hash/);
    expect((await call("PATCH", owner, { enabled: true })).status).toBe(200);
    expect((await call("DELETE")).status).toBe(204);
    expect(await (await call("GET")).json()).toEqual({ data: [] });
  });
});
