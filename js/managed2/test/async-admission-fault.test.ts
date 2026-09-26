import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("retains the live result observer after an alarm-read failure without replaying a tool", async () => {
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
    const original = state.storage.getAlarm.bind(state.storage);
    let first = true;
    state.storage.sql.exec("CREATE TABLE alarm_fault_probe (state TEXT NOT NULL)");
    Object.defineProperty(state.storage, "getAlarm", { configurable: true, value: async () => {
      if (first) {
        first = false;
        // The model may complete while admission awaits the alarm read. A
        // result observer must already be attached: redispatching after the
        // exception could repeat a side effect whose receipt is still live.
        let seen = "accepted";
        for (let i = 0; i < 100; i++) {
          seen = state.storage.sql.exec<{ state: string }>("SELECT state FROM turns LIMIT 1").toArray()[0]?.state ?? "missing";
          if (seen === "completed") break;
          await new Promise(resolve => setTimeout(resolve, 20));
        }
        state.storage.sql.exec("INSERT INTO alarm_fault_probe (state) VALUES (?)", seen);
        throw new Error("injected alarm read failure");
      }
      return original();
    } });
  });
  const turnId = crypto.randomUUID();
  const admitted = await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns`, {
    method: "POST", headers: { authorization, "content-type": "application/json", "idempotency-key": turnId },
    body: JSON.stringify({ input: "Use async web__run once, then report its result." }),
  });
  expect(admitted.status).toBe(503);
  const probe = await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{ state: string }>(
    "SELECT state FROM alarm_fault_probe",
  ).toArray()[0]!.state);
  expect(probe).toBe("completed");
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/turns/${turnId}`, { headers: { authorization } },
  )).json<{ state: string }>()).state, { timeout: 10_000, interval: 100 }).toBe("completed");
  const persisted = await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{ n: number }>(
    "SELECT COUNT(*) AS n FROM turns",
  ).toArray()[0]!.n);
  expect(persisted).toBe(1);
  await expect.poll(async () => await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{
    call_id: string; attempts: number; state: string;
  }>("SELECT call_id, attempts, state FROM async_jobs").toArray()), { timeout: 12_000, interval: 100 })
    .toMatchObject([{ call_id: "call-web", attempts: 1, state: "delivered" }]);
}, 20_000);
