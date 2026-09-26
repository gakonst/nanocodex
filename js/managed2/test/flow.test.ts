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

it("executes web__run end-to-end through credential-isolating Egress2 and resumes the model", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization },
    body: JSON.stringify({ input: "Use web__run once and summarize the search result." }) });
  expect(created.status).toBe(202);
  const { agent_id, turn_id } = await created.json<{ agent_id: string; turn_id: string }>();
  let result: { state: string; message: string; timing: { tool_calls: number }; tool_timing: { tool: string; status: string; phases: Record<string, { duration_ms: number }> }[] } | undefined;
  await expect.poll(async () => {
    const response = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } });
    result = await response.json<typeof result>();
    return result?.state;
  }, { timeout: 15_000 }).toBe("completed");
  expect(result?.message).toContain("[synthetic citation](https://example.org/source)");
  expect(result?.message).not.toContain("provider-only");
  expect(result?.timing.tool_calls).toBe(1);
  expect(result?.tool_timing).toHaveLength(1);
  expect(result?.tool_timing[0]?.tool).toBe("web__run");
  expect(result?.tool_timing[0]?.status).toBe("completed");
  for (const phase of ["handler", "preparation", "egress_dispatch", "egress_credential", "egress_upstream", "parse"]) {
    expect(result?.tool_timing[0]?.phases[phase]?.duration_ms).toBeGreaterThanOrEqual(0);
  }
}, 20_000);

it("routes subscription web__run through Egress2 without exposing account credentials to Managed2", async () => {
  const authorization = `Bearer ${fixtureKeys["subscription-owner"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/chatgpt", {
    method: "PUT", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ access_token: "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWZpeHR1cmUiLCJjaGF0Z3B0X2FjY291bnRfaXNfZmVkcmFtcCI6ZmFsc2V9fQ.fixture",
      refresh_token: "refresh-fixture-only", account_id: "account-fixture", expires_at: 4102444800000, fedramp: false }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization },
    body: JSON.stringify({ input: "Use web__run once and summarize the search result." }) });
  expect(created.status).toBe(202);
  const { agent_id, turn_id } = await created.json<{ agent_id: string; turn_id: string }>();
  let result: { state: string; message: string } | undefined;
  await expect.poll(async () => {
    result = await (await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } })).json<typeof result>();
    return result?.state;
  }, { timeout: 15_000 }).toBe("completed");
  expect(result?.message).toContain("[synthetic citation](https://example.org/source)");
}, 20_000);

it("anchors a new session relay to trusted SF ingress rather than an asserted client header", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "x-managed2-relay-region": "eeur" },
    cf: { colo: "SJC" },
  } as RequestInit);
  expect(created.status).toBe(201);
  const { agent_id: agentId } = await created.json<{ agent_id: string }>();
  const { env, runInDurableObject } = await import("cloudflare:test");
  const sessions = (env as unknown as { SESSIONS: DurableObjectNamespace }).SESSIONS;
  const stub = sessions.getByName(`fixture-user:${agentId}`);
  const region = await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{ relay_region: string | null }>(
    "SELECT relay_region FROM session_meta WHERE singleton = 1",
  ).toArray()[0]?.relay_region);
  expect(region).toBe("wnam");
});

it("opt-in async read finishes original turn before delayed web egress, then emits a tagged continuation", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const agentId = crypto.randomUUID();
  const create = () => SELF.fetch("https://api.test/v1/agents", { method: "POST",
    headers: { authorization, "idempotency-key": agentId },
    body: JSON.stringify({ input: "Use async web__run once and summarize the search result.", async_tools: true }) });
  const created = await create();
  expect(created.status).toBe(202);
  expect((await create()).status).toBe(202);
  const changed = await SELF.fetch("https://api.test/v1/agents", { method: "POST",
    headers: { authorization, "idempotency-key": agentId },
    body: JSON.stringify({ input: "Use async web__run once and summarize the search result.", async_tools: false }) });
  expect(changed.status).toBe(409);
  await expect.poll(async () => {
    const status = await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns/${agentId}`, { headers: { authorization } });
    return (await status.json<{ state: string; message: string }>()).state;
  }, { timeout: 10_000 }).toBe("completed");
  const original = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns/${agentId}`,
    { headers: { authorization } })).json<{ message: string }>();
  expect(original.message).toContain("in_progress");
  const jobsUrl = `https://api.test/v1/agents/${agentId}/jobs`;
  const jobs = await (await SELF.fetch(jobsUrl, { headers: { authorization } })).json<{
    job_id: string; state: string; result?: string }[]>();
  expect(jobs).toHaveLength(1);
  expect(["queued", "running"]).toContain(jobs[0]!.state);
  expect(jobs[0]!.result).toBeUndefined();
  const jobId = jobs[0]!.job_id;
  const foreign = `Bearer ${fixtureKeys["owner-no-key"]}`;
  expect((await SELF.fetch(`${jobsUrl}/${jobId}`, { headers: { authorization: foreign } })).status).toBe(404);
  const status = `${jobsUrl}/${jobId}`;
  let final: { job_id: string; state: string; result?: string; continuation_turn_id?: string } | undefined;
  await expect.poll(async () => {
    final = await (await SELF.fetch(status, { headers: { authorization } })).json<typeof final>();
    return final?.state;
  }, { timeout: 15_000, interval: 250 }).toBe("continued");
  expect(final?.result).toContain("synthetic citation");
  expect(final?.result).not.toContain("provider-only");
  await expect.poll(async () => {
    const turn = await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns/${final!.continuation_turn_id}`,
      { headers: { authorization } });
    const state = await turn.json<{ state: string; message?: string }>();
    return state.state === "completed" ? state.message : state.state;
  }, { timeout: 10_000 }).toContain("Background search finished");
  // Stable replay cannot launch another egress operation or create another job.
  expect((await create()).status).toBe(202);
  expect(await (await SELF.fetch(jobsUrl, { headers: { authorization } })).json()).toHaveLength(1);
}, 30_000);

it("does not activate async tool jobs by default and rejects non-boolean opt-in", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const invalid = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization },
    body: JSON.stringify({ input: "hello", async_tools: "yes" }) });
  expect(invalid.status).toBe(400);
  const ordinary = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization } });
  const { agent_id } = await ordinary.json<{ agent_id: string }>();
  expect((await SELF.fetch(`https://api.test/v1/agents/${agent_id}/jobs`, { headers: { authorization } })).status).toBe(404);
});

it("runs the second allowlisted read-only tool as a durable async job", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const created = await SELF.fetch("https://api.test/v1/agents", { method: "POST", headers: { authorization },
    body: JSON.stringify({ async_tools: true, input: "Use current_time exactly once and report the UTC timestamp it returns." }) });
  expect(created.status).toBe(202);
  const { agent_id, turn_id } = await created.json<{ agent_id: string; turn_id: string }>();
  await expect.poll(async () => (await (await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`,
    { headers: { authorization } })).json<{ state: string }>()).state, { timeout: 10_000 }).toBe("completed");
  let jobs: { state: string; tool: string; result?: string }[] = [];
  await expect.poll(async () => {
    jobs = await (await SELF.fetch(`https://api.test/v1/agents/${agent_id}/jobs`, { headers: { authorization } })).json<typeof jobs>();
    return jobs[0]?.state;
  }, { timeout: 10_000 }).toBe("continued");
  expect(jobs).toHaveLength(1);
  expect(jobs[0]!.tool).toBe("current_time");
  expect(jobs[0]!.result).toMatch(/utc.*\d{4}-\d{2}-\d{2}T/);
});
