import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("coalesces a completed tool and cancelled in-flight tool without replaying the side effect", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ input: "Use async mixed cohort: run two searches then report both.", async_tools: true }),
  });
  expect(created.status).toBe(202);
  const { agent_id: agentId, turn_id: turnId } = await created.json<{ agent_id: string; turn_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/turns/${turnId}`, { headers: { authorization } },
  )).json<{ state: string; message?: string }>()), { timeout: 15_000, interval: 100 })
    .toMatchObject({ state: "completed", message: "Waiting for mixed cohort" });
  const rows = await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{
    id: string; call_id: string; original_turn: string;
  }>("SELECT id, call_id, original_turn FROM async_jobs ORDER BY call_id").toArray());
  expect(rows.map(row => row.call_id)).toEqual(["call-mixed-a", "call-mixed-b"]);
  const fast = rows[0]!.id;
  const slow = rows[1]!.id;
  const url = (id: string) => `https://api.test/v1/agents/${agentId}/jobs/${id}`;
  const job = async (id: string) => (await SELF.fetch(url(id), { headers: { authorization } }))
    .json<{ state: string; result?: string; continuation_started?: boolean }>();
  await expect.poll(async () => (await job(slow)).state, { timeout: 1200, interval: 50 }).toBe("running");
  const cancelled = await SELF.fetch(url(slow), { method: "DELETE", headers: { authorization } });
  expect(cancelled.status).toBe(200);
  expect(await cancelled.json()).toMatchObject({ state: "uncertain",
    result: "Cancellation requested after dispatch; side effect may have occurred" });
  await expect.poll(async () => [(await job(fast)).state, (await job(slow)).state],
    { timeout: 20_000, interval: 100 }).toEqual(["delivered", "delivered"]);
  // Wait for the delayed upstream response: a late completion must not
  // replace the stable terminal result or spur another model request.
  await new Promise(resolve => setTimeout(resolve, 2600));
  expect(await job(slow)).toMatchObject({ state: "delivered", continuation_started: true,
    result: "Cancellation requested after dispatch; side effect may have occurred" });
  const final = await runInDurableObject(stub, (_session, state) => ({
    jobs: state.storage.sql.exec<{ call_id: string; terminal_state: string; attempts: number; original_turn: string }>(
      "SELECT call_id, terminal_state, attempts, original_turn FROM async_jobs ORDER BY call_id").toArray(),
    turns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(final.jobs).toMatchObject([
    { call_id: "call-mixed-a", terminal_state: "completed", attempts: 1, original_turn: turnId },
    { call_id: "call-mixed-b", terminal_state: "uncertain", attempts: 1, original_turn: turnId },
  ]);
  expect(final.turns).toBe(1);
}, 35_000);
