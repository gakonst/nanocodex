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
  const { agent_id, turn_id } = await created.json<{ agent_id: string; turn_id: string }>();
  async function completed(id: string): Promise<void> {
    await expect.poll(async () => {
      const response = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns/${id}`, { headers: { authorization } });
      return await response.json<{ state: string; message?: string }>();
    }, { timeout: 15_000 }).toMatchObject({ state: "completed", message: "hello from test model" });
  }
  await completed(turn_id);
  const next = await SELF.fetch(`https://api.test/v1/agents/${agent_id}/turns`, {
    method: "POST", headers: { authorization }, body: JSON.stringify({ input: "Second greeting" }),
  });
  expect(next.status).toBe(202);
  await completed((await next.json<{ turn_id: string }>()).turn_id);
});
