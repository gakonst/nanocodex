import assert from "node:assert/strict";
import test from "node:test";
import { Agent, ManagedError } from "../managed/index.mjs";

const agentId = "0198d3f0-8844-7000-8000-000000000001";
const apiKey = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
const trigger = {
  id: "morning", cron: "0 7 * * *", timezone: "Europe/Athens", input: "Daily summary",
  session_mode: "new", last_agent_id: null,
  enabled: true, next_run_at: 1_800_000, last_run_at: null, last_turn_id: null,
  last_skipped_at: null, created_at: 1, updated_at: 1,
};

test("cron SDK routes CRUD through the authenticated agent and returns immutable views", async () => {
  const calls = [];
  const agent = Agent.open(agentId, { baseUrl: "https://managed.example", apiKey, fetch: async (input, init) => {
    const request = new Request(input, init);
    assert.equal(request.headers.get("authorization"), `Bearer ${apiKey}`);
    assert.equal(request.credentials, "omit");
    calls.push([request.method, new URL(request.url).pathname, request.method === "PUT" ? await request.json() : null]);
    if (request.method === "DELETE") return new Response(null, { status: 204 });
    if (request.url.endsWith("/triggers")) return Response.json({ data: [trigger] });
    return Response.json({ ...trigger, authorization_json: "must not leak" });
  } });
  const config = { cron: trigger.cron, timezone: trigger.timezone, input: trigger.input, session_mode: "new" };
  const created = await agent.triggers.put("morning", config);
  assert.deepEqual(created, trigger);
  assert.ok(Object.isFrozen(created));
  assert.deepEqual(await agent.triggers.get("morning"), trigger);
  const listed = await agent.triggers.list();
  assert.deepEqual(listed, [trigger]);
  assert.ok(Object.isFrozen(listed));
  assert.ok(Object.isFrozen(listed[0]));
  await agent.triggers.put("morning", { ...config, enabled: false });
  await agent.triggers.delete("morning");
  assert.deepEqual(calls, [
    ["PUT", `/v1/agents/${agentId}/triggers/morning`, config],
    ["GET", `/v1/agents/${agentId}/triggers/morning`, null],
    ["GET", `/v1/agents/${agentId}/triggers`, null],
    ["PUT", `/v1/agents/${agentId}/triggers/morning`, { ...config, enabled: false }],
    ["DELETE", `/v1/agents/${agentId}/triggers/morning`, null],
  ]);
});

test("cron SDK rejects invalid ids and bodies before making a request", async () => {
  const agent = Agent.open(agentId, { baseUrl: "https://managed.example", fetch: () => { throw new Error("unexpected request"); } });
  for (const id of ["", "../turns", "a/b", "x".repeat(65)]) {
    await assert.rejects(agent.triggers.get(id), TypeError);
    await assert.rejects(agent.triggers.delete(id), TypeError);
  }
  for (const config of [{}, { cron: "* * * * * *", input: "text" },
    { cron: "* * * * *", input: " " }, { cron: "* * * * *", input: "x".repeat(65_537) },
    { cron: "* * * * *", input: "text", extra: true }, { cron: "* * * * *", input: "text", session_mode: "fork" }]) {
    await assert.rejects(agent.triggers.put("test", config), TypeError);
  }
});

test("cron SDK rejects malformed responses and preserves API errors", async () => {
  for (const body of [{}, { ...trigger, enabled: 1 }, { ...trigger, next_run_at: "tomorrow" },
    { ...trigger, last_turn_id: undefined }, { ...trigger, session_mode: "fork" }, { ...trigger, last_agent_id: "../other" }]) {
    const agent = Agent.open(agentId, { baseUrl: "https://managed.example", fetch: async () => Response.json(body) });
    await assert.rejects(agent.triggers.get("morning"), (e) => e instanceof ManagedError && e.code === "invalid_response");
  }
  const agent = Agent.open(agentId, { baseUrl: "https://managed.example", fetch: async () => Response.json({ error: "forbidden" }, { status: 403 }) });
  await assert.rejects(agent.triggers.list(), (e) => e instanceof ManagedError && e.status === 403);
});


test("cron SDK supports legacy views and both session modes", async () => {
  const { session_mode, last_agent_id, ...legacy } = trigger;
  const agent = Agent.open(agentId, { baseUrl: "https://managed.example", fetch: async (_url, init) => {
    if (init?.method === "PUT") {
      const body = JSON.parse(init.body);
      return Response.json({ ...trigger, session_mode: body.session_mode, last_agent_id: agentId });
    }
    return Response.json(legacy);
  } });
  assert.equal((await agent.triggers.get("morning")).session_mode, "continue");
  for (const mode of ["new", "continue"]) {
    const saved = await agent.triggers.put("morning", { cron: trigger.cron, input: trigger.input, session_mode: mode });
    assert.equal(saved.session_mode, mode);
    assert.equal(saved.last_agent_id, agentId);
  }
});


test("cron SDK accepts UUIDv8 session IDs created by idempotent scheduling", async () => {
  const childId = "4bcd45bc-209d-8df6-9ebd-a373f838c9ae";
  const agent = Agent.open(agentId, { baseUrl: "https://managed.example", fetch: async () => Response.json({
    ...trigger, last_agent_id: childId, last_turn_id: "cron:fixture:1788630780000", last_run_at: 1_800_000,
  }) });
  assert.equal((await agent.triggers.get("morning")).last_agent_id, childId);
});
