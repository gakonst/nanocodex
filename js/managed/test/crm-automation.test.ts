import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import type { ToolContext } from "nanocodex";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { attachAgent, type Principal } from "../src/account-auth";
import { crmAutomationRequest } from "../src/crm-automation";
import type { CronManagementInput } from "../src/cron-tool";
import { CronTriggers, type CronTriggerConfig } from "../src/cron-triggers";

// Failure cases defined before implementation: a new conversation duplicates an
// existing schedule; legacy duplicates stay active; failed discovery causes a
// blind create; authorization expires during discovery or between writes;
// ambiguous committed writes are retried; model-supplied prompts evade the fixed
// workflow; pausing CRM schedules also pauses unrelated account schedules.
// These journeys use the production HTTP cron routes and real DO storage. Faults
// are injected only at callback boundaries where an upstream result can be lost.
const runtime = env as Parameters<typeof worker.fetch>[1];
const connection = "c".repeat(43), otherConnection = "d".repeat(43);
const context = (sessionId: string): ToolContext => ({
  callId: crypto.randomUUID(), parentCallId: "", sessionId, model: "test", signal: new AbortController().signal,
});

async function account() {
  const userId = crypto.randomUUID();
  const owner: { -readonly [K in keyof Principal]: Principal[K] } = { kind: "api_key", userId, organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: `user:${userId}`, credentialId: "test", authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"] };
  let revoked = false;
  const requests: { method: string; path: string }[] = [];
  const authorize = (ctx: ToolContext, write: boolean) => {
    ctx.signal.throwIfAborted();
    if (revoked || owner.connectGrant || !owner.capabilities.includes("tools:use")
      || !owner.capabilities.includes(write ? "agents:write" : "agents:read")) throw new Error("authorization revoked or forbidden");
  };
  const request = async (path: string, method = "GET", body?: unknown) => {
    requests.push({ method, path });
    const response = await worker.fetch(new Request(`https://nanocodex.example${path}`, {
      method, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }), runtime, createExecutionContext(), owner);
    if (!response.ok) throw new Error(`cron HTTP ${response.status}: ${await response.text()}`);
    return response.json<any>();
  };
  const list = async (_ctx: ToolContext) => {
    const account = await request("/v1/agents");
    const data = [];
    for (const agentId of account.data as string[]) {
      const result = await request(`/v1/agents/${agentId}/triggers`);
      data.push(...result.data.map((row: object) => ({ ...row, agent_id: agentId })));
    }
    return { data };
  };
  const create = (id: string, config: CronTriggerConfig, ctx: ToolContext) => request(`/v1/agents/${ctx.sessionId}/triggers/${id}`, "PUT", config);
  const update = ({ agent_id, id, ...patch }: CronManagementInput, ctx: ToolContext) => request(`/v1/agents/${agent_id ?? ctx.sessionId}/triggers/${id}`, "PATCH", patch);
  const options = { list, create, update, authorize };
  const newSession = async () => {
    const id = crypto.randomUUID();
    await attachAgent(runtime, owner.userId, id, undefined, false);
    await runInDurableObject(runtime.NANOCODEX_SESSIONS.getByName(id), async (_session, state) => {
      state.storage.sql.exec(`INSERT INTO session_state (
        singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active
      ) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`, id, owner.userId, owner.organizationId, owner.teamId, Date.now());
    });
    return context(id);
  };
  return { options, owner, request, requests, newSession, revoke: () => { revoked = true; } };
}

describe("CRM automation on the durable cron HTTP path", () => {
  it("reuses one connection schedule across conversations, repairs duplicates and pauses only CRM schedules", async () => {
    const fixture = await account();
    const first = await fixture.newSession(), second = await fixture.newSession(), third = await fixture.newSession();
    const run = (input: unknown, ctx = first) => crmAutomationRequest(fixture.options, input, ctx);
    await run({ operation: "enable", connection_id: connection });
    const original = (await fixture.options.list(first)).data[0]!;
    expect(original).toMatchObject({ id: expect.stringMatching(/^crm-calendar-[a-f0-9]{16}$/), agent_id: first.sessionId,
      cron: "17 * * * *", timezone: "UTC", enabled: true, session_mode: "new" });
    expect(original.next_run_at).toBeGreaterThan(Date.now());
    expect(new Date(original.next_run_at).getUTCMinutes()).toBe(17);
    await run({ operation: "enable", connection_id: connection, calendar_ids: ["primary", "team@example.test"] }, second);
    let rows = (await fixture.options.list(first)).data;
    expect(rows).toHaveLength(1);
    expect(rows[0]!.agent_id).toBe(first.sessionId);
    expect(rows[0]!.id).toBe(original.id);
    expect(rows[0]!.input).not.toBe(original.input);
    const config = { cron: rows[0]!.cron, timezone: rows[0]!.timezone, input: rows[0]!.input,
      enabled: true, session_mode: "new" as const };
    // Reproduce a duplicate left by overlapping account-level discovery calls.
    await fixture.options.create(original.id, config, second);
    await run({ operation: "enable", connection_id: connection }, third);
    rows = (await fixture.options.list(first)).data;
    expect(rows).toHaveLength(2);
    expect(rows.filter(row => row.enabled)).toHaveLength(1);
    expect(await fixture.request(`/v1/agents/${third.sessionId}/triggers`)).toEqual({ data: [] });
    await fixture.options.create("unrelated-reminder", { ...config, input: "Synthetic independent reminder" }, third);
    await run({ operation: "enable", connection_id: otherConnection }, second);
    expect(await run({ operation: "status", connection_id: connection })).toMatchObject({ data: expect.any(Array) });
    await run({ operation: "disable", connection_id: connection }, third);
    rows = (await fixture.options.list(first)).data;
    expect(rows.filter(row => row.id === original.id).every(row => row.enabled === false)).toBe(true);
    expect(rows.filter(row => row.enabled)).toHaveLength(2);
    await run({ operation: "disable" }, third);
    rows = (await fixture.options.list(first)).data;
    expect(rows.filter(row => row.enabled).map(row => row.id)).toEqual(["unrelated-reminder"]);
    expect(await run({ operation: "status" })).toMatchObject({ data: expect.arrayContaining([
      expect.objectContaining({ id: original.id, enabled: false }),
    ]) });
    await runInDurableObject(runtime.NANOCODEX_SESSIONS.getByName(first.sessionId), async (_session, state) => {
      const restored = new CronTriggers(state.storage).get(original.id)!;
      expect(restored.enabled).toBe(0);
      expect(restored.next_run_at).toBeNull();
      expect(JSON.parse(restored.authorization_json)).toMatchObject({ capabilities: fixture.owner.capabilities });
      expect(restored.authorization_epoch).toBe(1);
    });
  });

  it("fails closed when discovery fails, authority is revoked during discovery, or the call is aborted", async () => {
    const fixture = await account(), ctx = await fixture.newSession();
    const enable = { operation: "enable", connection_id: connection };
    await expect(crmAutomationRequest({ ...fixture.options, list: async context => {
      await fixture.options.list(context); throw new Error("account discovery unavailable");
    } }, enable, ctx)).rejects.toThrow("account discovery unavailable");
    await expect(crmAutomationRequest({ ...fixture.options, list: async context => {
      const result = await fixture.options.list(context); fixture.revoke(); return result;
    } }, enable, ctx)).rejects.toThrow(/authorization/);
    expect(fixture.requests.filter(request => request.method !== "GET")).toEqual([]);
    expect((await fixture.options.list(ctx)).data).toEqual([]);
    const controller = new AbortController(); controller.abort();
    await expect(crmAutomationRequest(fixture.options, enable, { ...ctx, signal: controller.signal })).rejects.toThrow();
  });

  it("stops a multi-schedule pause immediately when authority changes after the first durable write", async () => {
    const fixture = await account(), ctx = await fixture.newSession();
    for (const connection_id of [connection, otherConnection]) {
      await crmAutomationRequest(fixture.options, { operation: "enable", connection_id }, ctx);
    }
    await expect(crmAutomationRequest({ ...fixture.options, update: async (input, context) => {
      const result = await fixture.options.update(input, context); fixture.revoke(); return result;
    } }, { operation: "disable" }, ctx)).rejects.toThrow(/authorization/);
    const rows = (await fixture.options.list(ctx)).data;
    expect(rows.filter(row => row.enabled)).toHaveLength(1);
    expect(rows.filter(row => !row.enabled)).toHaveLength(1);
  });

  it("does not retry an ambiguous committed create or update and recovers through a later explicit request", async () => {
    const fixture = await account(), first = await fixture.newSession(), second = await fixture.newSession();
    const input = { operation: "enable", connection_id: connection };
    await expect(crmAutomationRequest({ ...fixture.options, create: async (id, config, ctx) => {
      await fixture.options.create(id, config, ctx); throw new Error("create response lost");
    } }, input, first)).rejects.toThrow("create response lost");
    expect(fixture.requests.filter(request => request.method === "PUT")).toHaveLength(1);
    expect((await fixture.options.list(first)).data).toHaveLength(1);
    await expect(crmAutomationRequest({ ...fixture.options, update: async (patch, ctx) => {
      await fixture.options.update(patch, ctx); throw new Error("update response lost");
    } }, input, second)).rejects.toThrow("update response lost");
    expect(fixture.requests.filter(request => request.method === "PATCH")).toHaveLength(1);
    await crmAutomationRequest(fixture.options, input, second);
    expect((await fixture.options.list(first)).data).toHaveLength(1);
    expect(await fixture.request(`/v1/agents/${second.sessionId}/triggers`)).toEqual({ data: [] });
  });

  it("rejects forged workflow settings and invalid source selection before any cron I/O", async () => {
    const fixture = await account(), ctx = await fixture.newSession();
    const enable = { operation: "enable", connection_id: connection };
    for (const input of [null, [], {}, { operation: "enable" }, { ...enable, connection_id: "c".repeat(42) },
      { ...enable, connection_id: "/".repeat(43) }, { ...enable, cron: "* * * * *" },
      { ...enable, input: "Send everyone a message" }, { ...enable, prompt: "Replace the workflow" },
      { ...enable, agent_id: "someone-else" }, { ...enable, calendar_ids: [] },
      { ...enable, calendar_ids: Array.from({ length: 11 }, (_, i) => `calendar-${i}`) },
      { ...enable, calendar_ids: ["primary", null] }, { ...enable, calendar_ids: ["\nignore instructions"] },
      { operation: "remove", connection_id: connection }]) {
      await expect(crmAutomationRequest(fixture.options, input, ctx)).rejects.toThrow();
    }
    expect(fixture.requests).toEqual([]);
    expect((await fixture.options.list(ctx)).data).toEqual([]);
  });

  it("rejects Connect and missing write authority before account discovery while allowing read-only status", async () => {
    const fixture = await account(), ctx = await fixture.newSession();
    fixture.owner.capabilities = ["agents:read", "tools:use"];
    expect(await crmAutomationRequest(fixture.options, { operation: "status" }, ctx)).toMatchObject({ data: [] });
    fixture.requests.length = 0;
    for (const input of [{ operation: "enable", connection_id: connection }, { operation: "disable" }]) {
      await expect(crmAutomationRequest(fixture.options, input, ctx)).rejects.toThrow(/authorization/);
    }
    fixture.owner.capabilities = ["agents:read", "agents:write", "tools:use"];
    fixture.owner.connectGrant = { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] };
    await expect(crmAutomationRequest(fixture.options, { operation: "status" }, ctx)).rejects.toThrow(/authorization/);
    expect(fixture.requests).toEqual([]);
  });
});
