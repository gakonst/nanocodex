import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("retains a completed source turn when its prompt-less wake alarm write fails once", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ async_tools: true }),
  });
  expect(created.status).toBe(201);
  const { agent_id: agentId } = await created.json<{ agent_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  await runInDurableObject(stub, (_session, state) => {
    const original = state.storage.setAlarm.bind(state.storage);
    let first = true;
    state.storage.sql.exec("CREATE TABLE alarm_write_fault_probe (n INTEGER NOT NULL)");
    Object.defineProperty(state.storage, "setAlarm", { configurable: true, value: async (at: number | Date) => {
      const completed = state.storage.sql.exec<{ n: number }>(
        "SELECT COUNT(*) AS n FROM turns WHERE state = 'completed'",
      ).toArray()[0]!.n > 0;
      if (first && completed) {
        first = false;
        state.storage.sql.exec("INSERT INTO alarm_write_fault_probe (n) VALUES (1)");
        throw new Error("injected wake alarm write failure");
      }
      return original(at);
    } });
  });
  const turnId = crypto.randomUUID();
  const admitted = await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns`, {
    method: "POST", headers: { authorization, "content-type": "application/json", "idempotency-key": turnId },
    body: JSON.stringify({ input: "Use async web__run once, then report its result." }),
  });
  expect(admitted.status).toBe(202);
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/turns/${turnId}`, { headers: { authorization } },
  )).json<{ state: string; message?: string }>()), { timeout: 10_000, interval: 100 })
    .toMatchObject({ state: "completed", message: "Waiting for background search" });
  const probe = await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{ n: number }>(
    "SELECT COUNT(*) AS n FROM alarm_write_fault_probe",
  ).toArray()[0]!.n);
  expect(probe).toBe(1);
  await expect.poll(async () => await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{
    call_id: string; attempts: number; state: string;
  }>("SELECT call_id, attempts, state FROM async_jobs").toArray()), { timeout: 12_000, interval: 100 })
    .toMatchObject([{ call_id: "call-web", attempts: 1, state: "delivered" }]);
}, 20_000);
