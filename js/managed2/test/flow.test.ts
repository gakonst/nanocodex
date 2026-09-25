import { fixtureKeys } from "./fixtures/auth";
import { SELF } from "cloudflare:test";
import { expect, it } from "vitest";

it("admits a turn through API-key auth and the standard WASM Session DO", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const stored = await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ value: "sk-fixture-only" }),
  });
  expect(stored.status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization } });
  expect(created.status).toBe(201);
  expect(created.headers.get("server-timing")).toMatch(/auth;dur=.*session;dur=/);
  const { agent_id } = await created.json<{ agent_id: string }>();
  const key = crypto.randomUUID();
  const submitted = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns`, {
    method: "POST", headers: { authorization, "content-type": "application/json", "idempotency-key": key },
    body: JSON.stringify({ input: "Say hello" }),
  });
  expect(submitted.status).toBe(202);
  for (const phase of ["auth", "body_parse", "agent_init", "admission", "do_route", "do_total", "session", "api_total"])
    expect(submitted.headers.get("server-timing")).toContain(`${phase};dur=`);
  const { turn_id } = await submitted.json<{ turn_id: string }>();
  expect(turn_id).toBe(key);
  await expect.poll(async () => {
    const response = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } });
    return await response.json<{ state: string; message?: string }>();
  }, { timeout: 10_000 }).toMatchObject({ state: "completed", message: "hello from test model" });
  const timingResponse = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } });
  const observed = await timingResponse.json<{ timing: { trace_id: string; agent_init_ms: number;
    accepted_ms: number; first_delta_ms: number; first_answer_delta_ms: number | null; result_ms: number }; tool_timing: unknown[] }>();
  expect(observed.timing.trace_id).toMatch(/^[0-9a-f-]{36}$/);
  expect(observed.timing.agent_init_ms).toBeGreaterThanOrEqual(0);
  expect(observed.timing.first_delta_ms).toBeGreaterThanOrEqual(observed.timing.accepted_ms);
  expect(observed.timing.result_ms).toBeGreaterThanOrEqual(observed.timing.first_delta_ms);
  expect(observed.timing.first_answer_delta_ms).toBeNull(); // Synthetic fixture has no final_answer phase.
  expect(observed.tool_timing).toEqual([]);
  const repeat = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns`, {
    method: "POST", headers: { authorization, "idempotency-key": key },
    body: JSON.stringify({ input: "Say hello" }),
  });
  expect(await repeat.json()).toMatchObject({ turn_id: key, state: "completed" });
  const conflict = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns`, {
    method: "POST", headers: { authorization, "idempotency-key": key },
    body: JSON.stringify({ input: "Different prompt" }),
  });
  expect(conflict.status).toBe(409);
  const stream = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/events?cursor=0`, {
    headers: { authorization, upgrade: "websocket" },
  });
  expect(stream.status).toBe(101);
  stream.webSocket?.accept();
  stream.webSocket?.close();
});


it("does not let another owner read the session or borrow its model key", async () => {
  const writer = `Bearer ${fixtureKeys["owner-with-key"]}`;
  const other = `Bearer ${fixtureKeys["owner-no-key"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization: writer }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const made = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization: writer } });
  const { agent_id } = await made.json<{ agent_id: string }>();
  expect((await SELF.fetch(`https://api.test/v1/agents/${agent_id}`, { headers: { authorization: other } })).status).toBe(404);
  const otherAgent = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization: other } });
  const { agent_id: otherId } = await otherAgent.json<{ agent_id: string }>();
  const turn = await SELF.fetch(`https://api.test/v1/agents/${otherId}/turns`, {
    method: "POST", headers: { authorization: other }, body: JSON.stringify({ input: "hi" }),
  });
  expect(turn.status).toBe(202); // HTTP transport admits without a WebSocket credential preconnect.
  const { turn_id } = await turn.json<{ turn_id: string }>();
  await expect.poll(async () => {
    const response = await SELF.fetch(`https://api.test/v1/agents/${otherId}/turns/${turn_id}`, { headers: { authorization: other } });
    return (await response.json<{ state: string; message?: string }>()).state;
  }, { timeout: 10_000 }).toBe("failed");
});

it("uses a ChatGPT subscription without giving the access or refresh token to the Agent", async () => {
  const authorization = `Bearer ${fixtureKeys["subscription-owner"]}`;
  const stored = await SELF.fetch("https://api.test/v1/credentials/chatgpt", {
    method: "PUT", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ access_token: "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWZpeHR1cmUiLCJjaGF0Z3B0X2FjY291bnRfaXNfZmVkcmFtcCI6ZmFsc2V9fQ.fixture",
      refresh_token: "refresh-fixture-only", account_id: "account-fixture",
      expires_at: 4102444800000, fedramp: false }),
  });
  expect(stored.status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization } });
  const { agent_id } = await created.json<{ agent_id: string }>();
  const submitted = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns`, {
    method: "POST", headers: { authorization }, body: JSON.stringify({ input: "Say hello" }),
  });
  expect(submitted.status).toBe(202);
  const { turn_id } = await submitted.json<{ turn_id: string }>();
  await expect.poll(async () => {
    const response = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } });
    return await response.json<{ state: string; message?: string }>();
  }, { timeout: 10_000 }).toMatchObject({ state: "completed", message: "hello from test model" });
});

it("creates an agent and admits its first turn in one authenticated request, with stable retries", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const id = crypto.randomUUID();
  const url = "https://api.test/v1/agents";
  const headers = { authorization, "content-type": "application/json", "idempotency-key": id };
  const body = JSON.stringify({ input: "Combined hello" });
  const created = await SELF.fetch(url, { method: "POST", headers, body });
  expect(created.status).toBe(202);
  expect(created.headers.get("x-managed2-trace-id")).toMatch(/^[0-9a-f-]{36}$/);
  for (const phase of ["auth", "body_parse", "agent_init", "admission", "do_route", "do_total", "session", "api_total"])
    expect(created.headers.get("server-timing")).toContain(`${phase};dur=`);
  expect(await created.json()).toEqual({ agent_id: id, turn_id: id, state: "accepted" });
  const replay = await SELF.fetch(url, { method: "POST", headers, body });
  expect(replay.status).toBe(202);
  expect(await replay.json()).toMatchObject({ agent_id: id, turn_id: id });
  await expect.poll(async () => {
    const response = await SELF.fetch(`https://api.test/v1/agents/${id}/turns/${id}`, { headers: { authorization } });
    return await response.json<{ state: string; message?: string }>();
  }, { timeout: 10_000 }).toMatchObject({ state: "completed", message: "hello from test model" });
  const conflict = await SELF.fetch(url, { method: "POST", headers, body: JSON.stringify({ input: "Changed" }) });
  expect(conflict.status).toBe(409);
  expect(await conflict.json()).toEqual({ error: "idempotency_conflict" });
  const socket = await SELF.fetch(`https://api.test/v1/agents/${id}/events?cursor=0`, {
    headers: { authorization, upgrade: "websocket" },
  });
  expect(socket.status).toBe(101);
  socket.webSocket?.accept();
  socket.webSocket?.close();
});

it("validates combined create before allocating an agent and generates IDs when omitted", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const url = "https://api.test/v1/agents";
  const bad = await SELF.fetch(url, { method: "POST", headers: { authorization }, body: JSON.stringify({ input: " " }) });
  expect(bad.status).toBe(400);
  const invalidKey = await SELF.fetch(url, { method: "POST", headers: { authorization, "idempotency-key": "bad" },
    body: JSON.stringify({ input: "hi" }) });
  expect(invalidKey.status).toBe(400);
  const created = await SELF.fetch(url, { method: "POST", headers: { authorization },
    body: JSON.stringify({ input: "Combined hello" }) });
  expect(created.status).toBe(202);
  const { agent_id, turn_id } = await created.json<{ agent_id: string; turn_id: string }>();
  expect(agent_id).toMatch(/^[0-9a-f-]{36}$/);
  expect(turn_id).toBe(agent_id);
});


it("retries simultaneous combined create requests without duplicate turns", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const id = crypto.randomUUID();
  const calls = await Promise.all(Array.from({ length: 8 }, () => SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "idempotency-key": id },
    body: JSON.stringify({ input: "Simultaneous hello" }),
  })));
  expect(calls.map(call => call.status)).toEqual(Array(8).fill(202));
  for (const response of calls) expect(await response.json()).toMatchObject({ agent_id: id, turn_id: id });
});

it("rejects missing, malformed, and wrong API keys at the public Worker boundary", async () => {
  const url = "https://api.test/v1/agents";
  for (const authorization of [undefined, "Bearer invalid", `Bearer ncx2_${"Z".repeat(43)}`]) {
    const response = await SELF.fetch(url, {
      method: "POST", ...(authorization ? { headers: { authorization } } : {}),
    });
    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: "unauthorized" });
  }
});

it("admits a substantial prompt without an arbitrary JSON-body cap", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const id = crypto.randomUUID();
  const response = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "idempotency-key": id },
    body: JSON.stringify({ input: `summarize: ${"a".repeat(70_000)}` }),
  });
  expect(response.status).toBe(202);
  await expect.poll(async () => {
    const status = await SELF.fetch(`https://api.test/v1/agents/${id}/turns/${id}`, { headers: { authorization } });
    return (await status.json<{ state: string }>()).state;
  }, { timeout: 15_000 }).toBe("completed");
});

it("completes a real model-tool-model turn and reports separate tool and continuation timings", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ input: "Use current_time exactly once and report the UTC timestamp it returns." }),
  });
  expect(created.status).toBe(202);
  const { agent_id, turn_id } = await created.json<{ agent_id: string; turn_id: string }>();
  let status: { state: string; message?: string; timing: Record<string, number | null>;
    tool_timing: { call_id: string; tool: string; started_at: number; started_ms: number;
      result_ms: number; duration_ms: number; status: string;
      phases: Record<string, { duration_ms: number; count: number }> }[] } | undefined;
  await expect.poll(async () => {
    const response = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } });
    status = await response.json<typeof status>();
    return status?.state;
  }, { timeout: 10_000 }).toBe("completed");
  expect(status!.message).toMatch(/^Current UTC: \d{4}-\d\d-\d\dT/);
  expect(status!.timing.tool_calls).toBe(1);
  expect(status!.timing.first_tool_call_ms).toBeGreaterThanOrEqual(status!.timing.first_model_call_ms!);
  expect(status!.timing.first_tool_result_ms).toBeGreaterThanOrEqual(status!.timing.first_tool_call_ms!);
  expect(status!.timing.post_tool_model_call_ms).toBeGreaterThanOrEqual(status!.timing.first_tool_result_ms!);
  expect(status!.timing.result_ms).toBeGreaterThanOrEqual(status!.timing.post_tool_model_call_ms!);
  expect(status!.timing.tool_duration_ms).toBeGreaterThanOrEqual(0);
  expect(status!.tool_timing).toHaveLength(1);
  const observedTool = status!.tool_timing[0]!;
  expect(observedTool.tool).toBe("current_time");
  expect(observedTool.started_at).toBeGreaterThan(0);
  expect(observedTool.started_ms).toBeGreaterThanOrEqual(status!.timing.first_model_call_ms!);
  expect(observedTool.result_ms).toBeGreaterThanOrEqual(observedTool.started_ms);
  expect(observedTool.status).toBe("completed");
  expect(observedTool.duration_ms).toBeGreaterThanOrEqual(0);
  expect(observedTool.phases.handler.count).toBe(1);
  expect(observedTool.phases.handler.duration_ms).toBeGreaterThanOrEqual(0);
  expect(JSON.stringify(status!.tool_timing)).not.toContain("Use current_time");
  expect(JSON.stringify(status!.tool_timing)).not.toContain("Current UTC:");
  const events = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/events?cursor=0`, {
    headers: { authorization, upgrade: "websocket" },
  });
  expect(events.status).toBe(101);
  events.webSocket?.accept();
  events.webSocket?.close();
});
