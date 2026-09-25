import { SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";

it("completes two subscription turns through Egress2's WebSocket transport", async () => {
  const authorization = `Bearer ${fixtureKeys["subscription-owner"]}`;
  const stored = await SELF.fetch("https://api.test/v1/credentials/chatgpt", {
    method: "PUT", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({
      access_token: "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWZpeHR1cmUiLCJjaGF0Z3B0X2FjY291bnRfaXNfZmVkcmFtcCI6ZmFsc2V9fQ.fixture",
      refresh_token: "refresh-fixture-only", account_id: "account-fixture",
      expires_at: 4102444800000, fedramp: false,
    }),
  });
  expect(stored.status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json", "idempotency-key": crypto.randomUUID() },
    body: JSON.stringify({ input: "First greeting" }),
  });
  expect(created.status).toBe(202);
  const trace = created.headers.get("x-managed2-trace-id");
  expect(trace).toMatch(/^[0-9a-f-]{36}$/);
  const { agent_id, turn_id } = await created.json<{ agent_id: string; turn_id: string }>();
  async function completed(id: string): Promise<void> {
    await expect.poll(async () => {
      const response = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${id}`, { headers: { authorization } });
      return await response.json<{ state: string; message?: string }>();
    }, { timeout: 15_000 }).toMatchObject({ state: "completed", message: "hello from test model" });
  }
  await completed(turn_id);
  const firstStatus = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${turn_id}`, { headers: { authorization } });
  const firstTiming = (await firstStatus.json<{ timing: { trace_id: string; accepted_ms: number; first_delta_ms: number; result_ms: number } }>()).timing;
  expect(firstTiming.trace_id).toBe(trace);
  expect(firstTiming.result_ms).toBeGreaterThanOrEqual(firstTiming.accepted_ms);
  const next = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns`, {
    method: "POST", headers: { authorization }, body: JSON.stringify({ input: "Second greeting" }),
  });
  expect(next.status).toBe(202);
  const nextId = (await next.json<{ turn_id: string }>()).turn_id;
  await completed(nextId);
  const secondStatus = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${nextId}`, { headers: { authorization } });
  const secondTiming = (await secondStatus.json<{ timing: { trace_id: string; accepted_ms: number; result_ms: number } }>()).timing;
  expect(secondTiming.trace_id).not.toBe(firstTiming.trace_id);
  expect(secondTiming.result_ms).toBeGreaterThanOrEqual(secondTiming.accepted_ms);
});
