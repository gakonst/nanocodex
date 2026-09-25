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
  for (const phase of ["auth", "agent_init", "admission", "session"])
    expect(submitted.headers.get("server-timing")).toContain(`${phase};dur=`);
  const { turn_id } = await submitted.json<{ turn_id: string }>();
  expect(turn_id).toBe(key);
  await expect.poll(async () => {
    const response = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } });
    return await response.json<{ state: string; message?: string }>();
  }, { timeout: 10_000 }).toMatchObject({ state: "completed", message: "hello from test model" });
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
  for (const phase of ["auth", "agent_init", "admission", "session"])
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
  const calls = await Promise.all(Array.from({ length: 2 }, () => SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "idempotency-key": id },
    body: JSON.stringify({ input: "Simultaneous hello" }),
  })));
  expect(calls.map(call => call.status)).toEqual([202, 202]);
  for (const response of calls) expect(await response.json()).toMatchObject({ agent_id: id, turn_id: id });
});
