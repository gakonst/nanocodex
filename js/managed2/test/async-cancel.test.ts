import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("fences an in-flight cancelled tool as uncertain and delivers under the original ID", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ input: "Use async web__run once, then report its result.", async_tools: true }),
  });
  expect(created.status).toBe(202);
  const { agent_id: agentId, turn_id: turnId } = await created.json<{ agent_id: string; turn_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/turns/${turnId}`, { headers: { authorization } },
  )).json<{ state: string; message?: string }>()), { timeout: 10_000, interval: 100 })
    .toMatchObject({ state: "completed", message: "Waiting for background search" });
  const [initial] = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).json<{ job_id: string; state: string }[]>();
  expect(initial).toBeDefined();
  const jobId = initial!.job_id;
  const url = `https://api.test/v1/agents/${agentId}/jobs/${jobId}`;
  await expect.poll(async () => (await (await SELF.fetch(url, { headers: { authorization } }))
    .json<{ state: string }>()).state, { timeout: 1200, interval: 50 }).toBe("running");
  const cancelled = await SELF.fetch(url, { method: "DELETE", headers: { authorization } });
  expect(cancelled.status).toBe(200);
  expect(await cancelled.json()).toMatchObject({ job_id: jobId, state: "uncertain",
    result: "Cancellation requested after dispatch; side effect may have occurred" });
  const job = async () => (await SELF.fetch(url, { headers: { authorization } }))
    .json<{ state: string; result: string; continuation_started?: boolean }>();
  await expect.poll(async () => (await job()).state, { timeout: 25_000, interval: 100 }).toBe("delivered");
  // The late search completion cannot replace the cancellation fence.
  await new Promise(resolve => setTimeout(resolve, 2600));
  expect(await job()).toMatchObject({ state: "delivered", continuation_started: true,
    result: "Cancellation requested after dispatch; side effect may have occurred" });
  const persisted = await runInDurableObject(stub, (_session, state) => ({
    row: state.storage.sql.exec<{ terminal_state: string; attempts: number; call_id: string; original_turn: string }>(
      "SELECT terminal_state, attempts, call_id, original_turn FROM async_jobs WHERE id = ?", jobId).toArray()[0],
    turns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(persisted.row).toMatchObject({ terminal_state: "uncertain", attempts: 1,
    call_id: "call-web", original_turn: turnId });
  expect(persisted.turns).toBe(1);
}, 35_000);
