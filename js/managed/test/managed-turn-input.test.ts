import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { commitManagedTransition, type DurableAgentSession } from "../src/index";
import { DurableEventLog } from "../src/durable-events";
import type { ServerMessage } from "../src/protocol";
import { parseCommand } from "../src/protocol";
import { ManagedTurnArchive } from "../src/managed-turn-archive";
import { lazyTurnInput, readTurnInput, storeTurnInput } from "../src/managed-turn-input";

const bindings = env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  NANOCODEX_HISTORY: R2Bucket;
};

describe("chunked managed input", () => {
  it("accepts large inputs through the public text command contract", () => {
    const input = "x".repeat(2 * 1024 * 1024);
    expect(parseCommand(JSON.stringify({ type: "prompt", id: "large", input }))).toMatchObject({ input });
    expect(parseCommand(JSON.stringify({ type: "steer", id: "large", input }))).toMatchObject({ input });
  });

  it("admits and completes >2 MiB input/output, retries history projection after archival, and replays exact content", async () => {
    const stub = bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (session, state) => {
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      let projectionAvailable = false;
      let onProjection: (() => void) | undefined;
      const projections: Record<string, unknown>[] = [];
      Object.defineProperty(session, "env", { value: {
        ...runtimeEnv,
        NANOCODEX_MEMORY: { getByName: () => ({ fetch: async (url: string, options?: RequestInit) => {
          if (new URL(url).pathname === "/project") {
            if (!projectionAvailable) return new Response("fixture projection unavailable", { status: 503 });
            projections.push(JSON.parse(options!.body as string));
            onProjection?.();
          }
          return new Response(null, { status: 204 });
        } }) },
        NANOCODEX_ACCOUNT_TOOLS: { getByName: () => {
          throw Object.assign(new Error("fixture runtime temporarily unavailable"), { code: "retryable" });
        } },
      } });
      const now = Date.now();
      state.storage.sql.exec(`INSERT INTO session_state (
        singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
        public_origin, runtime_profile, last_active
      ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team', 1,
        'https://nanocodex.example/', 'managed', ?)`, crypto.randomUUID(), now);
      const input = 'a'.repeat(255_998) + '😀' + 'b'.repeat(2 * 1024 * 1024);
      const request = () => new Request("https://session.internal/turns", {
        method: "POST", headers: { "Idempotency-Key": "large-input" },
        body: JSON.stringify({ id: "large-input", input }),
      });
      const admitted = await session.fetch(request());
      expect(admitted.status).toBe(202);
      expect(await admitted.json()).toMatchObject({ turn_id: "large-input", input });
      const row = state.storage.sql.exec<{ input_json: string }>(
        "SELECT input_json FROM managed_turns WHERE id = 'large-input'",
      ).one();
      expect(row.input_json).toMatch(/^chunks:/);
      expect(readTurnInput(state.storage, "large-input", row.input_json)).toBe(JSON.stringify(input));
      expect(state.storage.sql.exec<{ bytes: number }>(
        "SELECT MAX(LENGTH(CAST(input_json AS BLOB))) AS bytes FROM managed_turn_input_chunks",
      ).one().bytes).toBeLessThan(1_024_000);
      expect(state.storage.sql.exec<{ first_prompt: string }>(
        "SELECT first_prompt FROM session_state WHERE singleton = 1",
      ).one().first_prompt.length).toBeLessThanOrEqual(56);
      const replay = await session.fetch(request());
      expect(replay.status).toBe(200);
      expect(await replay.json()).toMatchObject({ input });
      // Exercise the production transaction, not a direct UPDATE that skips
      // terminal/outbox storage. Both input and result exceed SQLite's row limit.
      const finalMessage = "output😀".repeat(310_000);
      const terminal = { type: "turn_completed" as const, id: "large-input", final_message: finalMessage, usage: null, citations: [] };
      const log = new DurableEventLog<Extract<ServerMessage, { type: "turn_completed" }>>(state.storage);
      const completion = commitManagedTransition(state.storage, log, "large-input", terminal);
      expect(completion.committed.state).toBe("completed");
      expect(JSON.parse(completion.committed.terminal_json!)).toEqual(terminal);
      expect(state.storage.sql.exec<{ terminal_json: string }>("SELECT terminal_json FROM managed_turns WHERE id = 'large-input'").one().terminal_json).toMatch(/^chunks:/);
      expect(state.storage.sql.exec<{ payload_json: string }>("SELECT payload_json FROM history_projection_outbox WHERE turn_id = 'large-input'").one().payload_json).toMatch(/^chunks:/);
      expect(state.storage.sql.exec<{ completed_turns: number }>("SELECT completed_turns FROM session_state").one().completed_turns).toBe(1);
      commitManagedTransition(state.storage, log, "large-input", terminal);
      expect(state.storage.sql.exec<{ completed_turns: number }>("SELECT completed_turns FROM session_state").one().completed_turns).toBe(1);
      await expect(session.alarm()).rejects.toThrow("memory projection failed with HTTP 503");
      const archive = new ManagedTurnArchive(state.storage, bindings.NANOCODEX_HISTORY, state.id.toString());
      const failing = new ManagedTurnArchive(state.storage, {
        put: async () => { throw new Error("fixture R2 unavailable"); },
      } as unknown as R2Bucket, state.id.toString());
      await expect(failing.seal(true, 0)).rejects.toThrow("fixture R2 unavailable");
      expect(readTurnInput(state.storage, "large-input", row.input_json)).toBe(JSON.stringify(input));
      expect(await archive.seal(true, 0)).toMatchObject({ sealed: true, archived_receipts: 1 });
      expect(state.storage.sql.exec<{ count: number }>(
        "SELECT COUNT(*) AS count FROM managed_turn_input_chunks",
      ).one().count).toBe(0);
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_turn_terminal_chunks").one().count).toBe(0);
      expect(JSON.parse((await archive.findById("large-input"))!.terminal_json)).toEqual(terminal);
      expect(JSON.parse((await archive.findById("large-input"))!.input_json)).toBe(input);
      expect(JSON.parse((await archive.findByRequestKey("large-input"))!.input_json)).toBe(input);
      const archivedReplay = await session.fetch(request());
      expect(archivedReplay.status).toBe(200);
      expect(await archivedReplay.json()).toMatchObject({ input, state: "completed", terminal });
      projectionAvailable = true;
      state.storage.sql.exec("UPDATE history_projection_outbox SET retry_at = 0");
      await session.alarm();
      expect(projections).toHaveLength(1);
      expect(projections[0]).toMatchObject({ input, final_message: finalMessage, turn_id: "large-input" });
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_history_projection_chunks").one().count).toBe(0);
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM history_projection_outbox").one().count).toBe(0);
      // An old projection acknowledgement cannot delete a newer completion.
      expect((await session.fetch(new Request("https://session.internal/turns", {
        method: "POST", body: JSON.stringify({ id: "projection-race", input: "race" }),
      }))).status).toBe(202);
      const first = { ...terminal, id: "projection-race", final_message: "earlier completion" };
      commitManagedTransition(state.storage, log, first.id, first);
      onProjection = () => {
        onProjection = undefined;
        state.storage.sql.exec("UPDATE managed_turns SET state = 'accepted' WHERE id = 'projection-race'");
        commitManagedTransition(state.storage, log, first.id, { ...first, final_message: finalMessage });
      };
      await session.alarm();
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM history_projection_outbox").one().count).toBe(1);
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_history_projection_chunks").one().count).toBeGreaterThan(0);
      await session.alarm();
      expect(projections.at(-1)).toMatchObject({ turn_id: "projection-race", final_message: finalMessage });
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM history_projection_outbox").one().count).toBe(0);
      const conflict = await session.fetch(new Request("https://session.internal/turns", {
        method: "POST", headers: { "Idempotency-Key": "large-input" },
        body: JSON.stringify({ id: "large-input", input: input + "different" }),
      }));
      expect(conflict.status).toBe(409);
      await state.storage.deleteAlarm();
    });
  });

  it("hydrates each retained row only once", async () => {
    await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID()), async (_session, state) => {
      const input = JSON.stringify("x".repeat(300_000));
      const reference = storeTurnInput(state.storage, "memo", input);
      const row = lazyTurnInput(state.storage, { id: "memo", input_json: reference });
      expect(row.input_json).toBe(input);
      state.storage.sql.exec("DELETE FROM managed_turn_input_chunks WHERE turn_id = 'memo'");
      expect(row.input_json).toBe(input);
    });
  });

  it("defers hydration and fails closed on missing chunks", async () => {
    await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID()), async (_session, state) => {
      const input = JSON.stringify('x'.repeat(2 * 1024 * 1024));
      const reference = storeTurnInput(state.storage, "lazy", input);
      const row = lazyTurnInput(state.storage, { id: "lazy", input_json: reference, state: "accepted" });
      state.storage.sql.exec("DELETE FROM managed_turn_input_chunks WHERE turn_id = 'lazy' AND chunk_index = 2");
      expect(row.state).toBe("accepted");
      expect(() => row.input_json).toThrow(/input chunks/);
    });
  });
});
