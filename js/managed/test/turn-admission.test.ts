import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import type { DurableAgentSession } from "../src/index";

const FIXTURE_UNFINISHED_TURNS = 20;

describe("managed durable turn admission", () => {
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
    });
  });
});
