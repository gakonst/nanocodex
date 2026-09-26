import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("injects a completed original-call result at the active turn's next model boundary", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ input: "Use async active boundary: run the search and keep working.", async_tools: true }),
  });
  expect(created.status).toBe(202);
  const { agent_id: agentId, turn_id: turnId } = await created.json<{ agent_id: string; turn_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  let turn: { state: string; message?: string; timing?: { result_ms: number } } | undefined;
  await expect.poll(async () => {
    turn = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns/${turnId}`, {
      headers: { authorization },
    })).json();
    return turn;
  }, { timeout: 15_000, interval: 100 }).toMatchObject({
    state: "completed", message: expect.stringContaining("[active fixture]"),
  });
  expect(turn!.message).toMatch(/^Active boundary terminal: /);
  const jobs = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).json<{ job_id: string; tool: string; state: string }[]>();
  expect(jobs).toHaveLength(2);
  const webId = jobs.find(job => job.tool === "web__run")?.job_id;
  expect(webId).toBeDefined();
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/jobs/${webId}`, {
      headers: { authorization },
    })).json<{ state: string }>()).state,
  { timeout: 15_000, interval: 100 }).toBe("delivered");
  const rows = await runInDurableObject(stub, (_session, state) => ({
    jobs: state.storage.sql.exec<{ id: string; call_id: string; original_turn: string; state: string }>(
      "SELECT id, call_id, original_turn, state FROM async_jobs ORDER BY call_id").toArray(),
    userTurns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(rows.jobs.map(job => job.call_id)).toEqual(["call-active-time", "call-active-web"]);
  expect(rows.jobs.every(job => job.original_turn === turnId)).toBe(true);
  expect(rows.userTurns).toBe(1);
  expect(turn!.timing!.result_ms).toBeGreaterThanOrEqual(2200);
}, 30_000);
