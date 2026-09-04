import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { DurableAgentSession } from "../src/index";

const FIXTURE_UNFINISHED_TURNS = 20;

describe("managed durable turn admission", () => {
  for (const connected of [false, true]) {
    it(`retains a recovery alarm during stalled admission with connected=${connected}`, async () => {
      const sessions = (env as unknown as {
        NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
      }).NANOCODEX_SESSIONS;
      await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
        const discovery = Promise.withResolvers<Response>();
        const entered = Promise.withResolvers<void>();
        const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
        Object.defineProperty(session, "env", { value: {
          ...runtimeEnv,
          NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: () => {
            entered.resolve();
            return discovery.promise;
          } }) },
        } });
        const now = Date.now();
        state.storage.sql.exec(
          `INSERT INTO session_state (
             singleton, session_id, owner_id, organization_id, team_id,
             authorization_epoch, public_origin, runtime_profile, last_active
           ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team',
                     1, 'https://nanocodex.example/', 'managed', ?)`,
          crypto.randomUUID(), now - 120_000,
        );
        const pair = connected ? new WebSocketPair() : undefined;
        if (pair) {
          state.acceptWebSocket(pair[1], ["client"]);
          pair[0].accept();
        }
        try {
          const accepted = await session.fetch(new Request("https://session.internal/turns", {
            method: "POST", body: JSON.stringify({ id: "stalled", input: "test admission" }),
          }));
          expect(accepted.status).toBe(202);
          await entered.promise;
          await session.alarm();
          const alarm = await state.storage.getAlarm();
          expect(alarm).toBeGreaterThanOrEqual(Date.now() + 59_000);
          expect(alarm).toBeLessThanOrEqual(Date.now() + 60_000);
        } finally {
          discovery.resolve(Response.json({ tools: [], machines: [] }));
          pair?.[0].close(1000, "test complete");
          pair?.[1].close(1000, "test complete");
        }
      });
    });
  }

  it("accepts another turn when more than 16 durable turns are unfinished", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    const stub = sessions.getByName(crypto.randomUUID());

    await runInDurableObject(stub, async (session, state) => {
      const now = Date.now();
      const retryAt = now + 60_000;
      state.storage.sql.exec(
        `INSERT INTO session_state (
           singleton, session_id, owner_id, organization_id, team_id,
           authorization_epoch, public_origin, runtime_profile, accepted_turns,
           completed_turns, first_prompt, last_active
         ) VALUES (1, ?, ?, ?, ?, 1, ?, 'managed', ?, 0, ?, ?)`,
        "01992222-2222-7222-8222-222222222222",
        "fixture-owner",
        "fixture-organization",
        "fixture-team",
        "https://nanocodex.example/",
        FIXTURE_UNFINISHED_TURNS,
        "fixture prompt",
        now,
      );
      for (let index = 0; index < FIXTURE_UNFINISHED_TURNS; index += 1) {
        state.storage.sql.exec(
          `INSERT INTO managed_turns (
             id, request_key, request_hash, input_json, authorization_json,
             state, accepted_cursor, may_have_inner_operation, attempt_count,
             retry_at, created_at, accepted_at, updated_at
           ) VALUES (?, ?, ?, ?, ?, 'accepted', 0, 0, 1, ?, ?, ?, ?)`,
          `fixture-${index}`,
          `fixture-${index}`,
          `hash-${index}`,
          JSON.stringify(`fixture prompt ${index}`),
          JSON.stringify({ capabilities: [] }),
          retryAt,
          now - FIXTURE_UNFINISHED_TURNS + index,
          now - FIXTURE_UNFINISHED_TURNS + index,
          now,
        );
      }

      const response = await session.fetch(new Request("https://session.internal/turns", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ id: "accepted-beyond-sixteen", input: "queued prompt" }),
      }));
      const body = await response.json<{ error?: string; state?: string; turn_id?: string }>();

      expect(response.status).toBe(202);
      expect(body).toMatchObject({
        state: "accepted",
        turn_id: "accepted-beyond-sixteen",
      });
      expect(body.error).not.toBe("turn_queue_full");
      expect(state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_turns WHERE state IN ('accepted', 'cancelling')",
      ).one().count).toBe(FIXTURE_UNFINISHED_TURNS + 1);

      // Reconnect sees accepted work even before a runtime handle exists.
      const status = await session.fetch(new Request("https://session.internal/state"));
      const retained = await status.json<{
        active_turns: string[];
        active_turn_details: { id: string; input: string }[];
        agent_loaded: boolean;
      }>();
      expect(retained.agent_loaded).toBe(false);
      expect(retained.active_turns).toEqual([
        ...Array.from({ length: FIXTURE_UNFINISHED_TURNS }, (_, index) => `fixture-${index}`),
        "accepted-beyond-sixteen",
      ]);
      expect(retained.active_turn_details.at(-1)).toEqual({
        id: "accepted-beyond-sixteen", input: "queued prompt",
      });

      const steer = await session.fetch(new Request("https://session.internal/turns/fixture-0/steer", {
        method: "POST",
        body: JSON.stringify({ input: "preserve this instruction" }),
      }));
      expect(steer.status).toBe(503);
      expect(await steer.json()).toMatchObject({ error: "turn_recovering" });
      expect(state.storage.sql.exec<{ retry_at: number }>(
        "SELECT retry_at FROM managed_turns WHERE id = 'fixture-0'",
      ).one().retry_at).toBe(retryAt);
      expect(await state.storage.getAlarm()).not.toBeNull();

    });
  });
});
