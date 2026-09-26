import { env, evictDurableObject, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

// The persistent WebSocket test transport intentionally retains an active
// socket reference; Miniflare cannot evict that DO. This eviction fault test
// uses the HTTP transport, while async-e2e covers both transport paths.
it("reconciles a lost SQL job completion after DO eviction under its original call ID", async () => {
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
  const turnUrl = `https://api.test/v1/agents/${agentId}/turns/${turnId}`;
  await expect.poll(async () => (await (await SELF.fetch(turnUrl, { headers: { authorization } }))
    .json<{ state: string; message?: string }>()), { timeout: 15_000, interval: 100 })
    .toMatchObject({ state: "completed", message: "Waiting for background search" });
  const [initial] = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).json<{ job_id: string; state: string }[]>();
  expect(initial).toBeDefined();
  const jobId = initial!.job_id;
  const job = async () => (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs/${jobId}`, {
    headers: { authorization },
  })).json<{ state: string; continuation_started?: boolean }>();
  await expect.poll(async () => (await job()).state, { timeout: 30_000, interval: 100 }).toBe("delivered");
  // Inject a crash window: the typed terminal was consumed by a model step,
  // but the host's SQL completion marker was lost. The read-only tool may be
  // replayed after its lease expires; the durable Rust receipt must dedupe the
  // *same* call/job ID, without creating a prompt-bearing user turn.
  await runInDurableObject(stub, async (_session, state) => {
    state.storage.sql.exec(`UPDATE async_jobs SET state = 'running', result = NULL,
      terminal_state = NULL, delivered_at = NULL, continuation_started = NULL,
      wake_generation = -1, started_at = ?, lease_id = 'simulated-lost-lease'
      WHERE id = ? AND state = 'delivered'`, Date.now() - 31_000, jobId);
    await state.storage.setAlarm(Date.now() + 1_000);
  });
  await evictDurableObject(stub);
  await expect.poll(async () => (await job()).state, { timeout: 30_000, interval: 100 }).toBe("delivered");
  expect(await job()).toMatchObject({ state: "delivered", continuation_started: true });
  const final = await runInDurableObject(stub, (_session, state) => ({
    row: state.storage.sql.exec<{ call_id: string; attempts: number; original_turn: string }>(
      "SELECT call_id, attempts, original_turn FROM async_jobs WHERE id = ?", jobId).toArray()[0],
    turns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(final.row).toMatchObject({ call_id: "call-web", original_turn: turnId });
  expect(final.row.attempts).toBeGreaterThanOrEqual(2);
  expect(final.turns).toBe(1);
}, 40_000);

it("rearms a lost terminal-job alarm on real DO eviction and dedupes the original native receipt", async () => {
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
  const jobsUrl = `https://api.test/v1/agents/${agentId}/jobs`;
  await expect.poll(async () => (await (await SELF.fetch(jobsUrl, { headers: { authorization } }))
    .json<{ job_id: string }[]>()).length, { timeout: 15_000, interval: 100 }).toBe(1);
  const [job] = await (await SELF.fetch(jobsUrl, { headers: { authorization } }))
    .json<{ job_id: string }[]>();
  const status = async () => (await (await SELF.fetch(`${jobsUrl}/${job!.job_id}`, {
    headers: { authorization },
  })).json<{ state: string; result?: string }>()).state;
  await expect.poll(status, { timeout: 30_000, interval: 100 }).toBe("delivered");
  // The native step completed, but the host's terminal SQL transition and
  // alarm were lost in a crash. A cold constructor must rearm even if no user
  // turn remains active; stable operation-ID replay cannot produce a new turn.
  await runInDurableObject(stub, async (_session, state) => {
    state.storage.sql.exec(`UPDATE async_jobs SET state = 'completed', delivered_at = NULL,
      continuation_started = NULL, wake_generation = -1 WHERE id = ? AND state = 'delivered'`, job!.job_id);
    await state.storage.deleteAlarm();
  });
  await evictDurableObject(stub);
  await expect.poll(status, { timeout: 30_000, interval: 100 }).toBe("delivered");
  const final = await runInDurableObject(stub, (_session, state) => ({
    row: state.storage.sql.exec<{ call_id: string; original_turn: string }>(
      "SELECT call_id, original_turn FROM async_jobs WHERE id = ?", job!.job_id).toArray()[0],
    turns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(final.row).toMatchObject({ call_id: "call-web", original_turn: turnId });
  expect(final.turns).toBe(1);
}, 45_000);
