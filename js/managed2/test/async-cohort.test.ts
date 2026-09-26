import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("coalesces two same-turn completed jobs in one prompt-less wake with original call IDs", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ input: "Use async cohort: run two searches then report both.", async_tools: true }),
  });
  expect(created.status).toBe(202);
  const { agent_id: agentId, turn_id: turnId } = await created.json<{ agent_id: string; turn_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/turns/${turnId}`, { headers: { authorization } },
  )).json<{ state: string; message?: string }>()), { timeout: 15_000, interval: 100 })
    .toMatchObject({ state: "completed", message: "Waiting for coalesced background searches" });
  const jobs = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).json<{ job_id: string; state: string }[]>();
  expect(jobs).toHaveLength(2);
  await expect.poll(async () => Promise.all(jobs.map(async job => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/jobs/${job.job_id}`, { headers: { authorization } },
  )).json<{ state: string }>()).state)), { timeout: 20_000, interval: 100 })
    .toEqual(["delivered", "delivered"]);
  const rows = await runInDurableObject(stub, (_session, state) => ({
    jobs: state.storage.sql.exec<{ call_id: string; original_turn: string; continuation_started: number }>(
      "SELECT call_id, original_turn, continuation_started FROM async_jobs ORDER BY call_id").toArray(),
    turns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(rows.jobs.map(job => job.call_id)).toEqual(["call-cohort-a", "call-cohort-b"]);
  expect(rows.jobs.every(job => job.original_turn === turnId && job.continuation_started === 1)).toBe(true);
  expect(rows.turns).toBe(1);
}, 30_000);
