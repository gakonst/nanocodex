import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("returns same-ID pending without waiting for a slow tool, then wakes for same-ID terminal", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  const stored = await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  });
  expect(stored.status).toBe(204);
  const start = Date.now();
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ input: "Use async web__run once, then report its result.", async_tools: true }),
  });
  expect(created.status).toBe(202);
  const { agent_id: agentId, turn_id: turnId } = await created.json<{ agent_id: string; turn_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  const turnUrl = `https://api.test/v1/agents/${agentId}/turns/${turnId}`;
  let turn: { state: string; message?: string };
  await expect.poll(async () => {
    turn = await (await SELF.fetch(turnUrl, { headers: { authorization } })).json();
    return turn;
  }, { timeout: 15_000, interval: 100 }).toMatchObject({ state: "completed", message: "Waiting for background search" });
  const originalTurnMs = Date.now() - start;
  const jobs = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).json<{ job_id: string; state: string }[]>();
  expect(jobs).toHaveLength(1);
  const jobId = jobs[0]!.job_id;
  const job = async () => (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs/${jobId}`, {
    headers: { authorization },
  })).json<{ state: string; continuation_started?: boolean; result?: string }>();
  await expect.poll(async () => (await job()).state, { timeout: 30_000, interval: 100 }).toBe("delivered");
  expect(await job()).toMatchObject({ state: "delivered", continuation_started: true });
  const rows = await runInDurableObject(stub, (_session, state) => ({
    source: state.storage.sql.exec<{ state: string; message: string }>(
      "SELECT state, message FROM turns WHERE id = ?", turnId).toArray()[0],
    asyncJob: state.storage.sql.exec<{ state: string; call_id: string; original_turn: string; execution_turn: string }>(
      "SELECT state, call_id, original_turn, execution_turn FROM async_jobs WHERE id = ?", jobId).toArray()[0],
    // A background wake must not be a fabricated prompt-bearing user turn.
    userTurns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(rows.source).toMatchObject({ state: "completed", message: "Waiting for background search" });
  expect(rows.asyncJob).toMatchObject({ state: "delivered", call_id: "call-web", original_turn: turnId });
  expect(rows.userTurns).toBe(1);
  expect(originalTurnMs).toBeLessThan(2500);
}, 40_000);
