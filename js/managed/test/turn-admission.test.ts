import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";

import type { DurableAgentSession } from "../src/index";
import { ArchiveMaintenance } from "../src/archive-maintenance";
import { DurableEventLog } from "../src/durable-events";

const FIXTURE_UNFINISHED_TURNS = 20;

// Gate a mandatory startup step, not optional account-hand inventory.
function admissionBinding(bind: () => Response | Promise<Response>) {
  return { fetch: (input: RequestInfo | URL) => {
    const url = new URL(input instanceof Request ? input.url : String(input));
    if (url.pathname.startsWith("/subjects/")) return bind();
    return Promise.resolve(Response.json({ connectors: {}, mcp_connections: [] }));
  } };
}

describe("managed durable turn admission", () => {
  it("preserves an accepted legacy model across cold restart and rejects before host startup", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      let hostRequests = 0;
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      Object.defineProperty(session, "env", { value: {
        ...runtimeEnv,
        NANOCODEX: { fetch: async () => {
          hostRequests++;
          throw new Error("legacy continuation reached the host binding");
        } },
      } });
      const now = Date.now();
      state.storage.sql.exec(
        `INSERT INTO session_state (
           singleton, session_id, owner_id, organization_id, team_id,
           authorization_epoch, public_origin, runtime_profile, last_active
         ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team',
                   1, 'https://nanocodex.example/', 'managed', ?)`,
        crypto.randomUUID(),
        now,
      );
      state.storage.sql.exec(
        "INSERT INTO managed_configuration VALUES (1, ?)",
        JSON.stringify({
          tools: [],
          environment: { files: [], skills: [], setup_commands: [], network: { access: "disabled" } },
        }),
      );
      state.storage.sql.exec(
        "UPDATE managed_agent_settings SET model = 'gpt-5.6-luna', thinking = 'none'",
      );
      state.storage.sql.exec(
        `INSERT INTO managed_turns (
           id, request_hash, input_json, authorization_json, state,
           accepted_cursor, dispatch_input_chunks, may_have_inner_operation,
           attempt_count, created_at, accepted_at, updated_at
         ) VALUES ('legacy-resume', 'hash', '"continue"', '{"capabilities":[]}',
                   'accepted', 0, 1, 0, 0, ?, ?, ?)`,
        now,
        now,
        now,
      );
      state.storage.sql.exec(
        "INSERT INTO managed_turn_dispatch_chunks VALUES ('legacy-resume', 0, '\"continue\"')",
      );

      await session.alarm();

      expect(state.storage.sql.exec<{ model: string; thinking: string }>(
        "SELECT model, thinking FROM managed_agent_settings WHERE singleton = 1",
      ).one()).toEqual({ model: "gpt-5.6-luna", thinking: "none" });
      await expect.poll(() => state.storage.sql.exec<{ state: string; error: string | null }>(
        "SELECT state, error FROM managed_turns WHERE id = 'legacy-resume'",
      ).one()).toMatchObject({ state: "failed", error: expect.stringContaining("no longer supported") });
      expect(hostRequests).toBe(0);
    });
  });

  it("continues cold admission while optional hand discovery is stalled or fails", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      const discovery = Promise.withResolvers<Response>();
      const mandatoryStartup = Promise.withResolvers<void>();
      const binding = Promise.withResolvers<Response>();
      Object.defineProperty(session, "env", { value: {
        ...runtimeEnv,
        NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: () => discovery.promise }) },
        NANOCODEX: admissionBinding(() => {
          mandatoryStartup.resolve();
          return binding.promise;
        }),
      } });
      state.storage.sql.exec(`INSERT INTO session_state (
        singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
        public_origin, runtime_profile, last_active
      ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team', 1,
        'https://nanocodex.example/', 'managed', ?)`, crypto.randomUUID(), Date.now());
      try {
        const response = await session.fetch(new Request("https://session.internal/turns", {
          method: "POST", body: JSON.stringify({ id: "independent", input: "brain-only task" }),
        }));
        expect(response.status).toBe(202);
        // This mandatory construction step was previously unreachable until
        // account hand discovery completed, even for an unrelated task.
        await mandatoryStartup.promise;
        discovery.resolve(new Response(null, { status: 503 }));
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(state.storage.sql.exec<{ state: string; error: string | null }>(
          "SELECT state, error FROM managed_turns WHERE id = 'independent'",
        ).one()).toEqual({ state: "accepted", error: null });
      } finally {
        discovery.resolve(Response.json({ tools: [], machines: [] }));
        state.storage.sql.exec("UPDATE managed_turns SET state = 'cancelled', retry_at = NULL WHERE id = 'independent'");
        // End before model execution: the test owns only the admission boundary.
        binding.resolve(new Response(null, { status: 403 }));
        await state.storage.deleteAlarm();
      }
    });
  });

  it("retains more than 64 pre-admission cancellations and consumes only the matching turn", async () => {
    const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      Object.defineProperty(session, "env", { value: { ...runtimeEnv,
        NANOCODEX: admissionBinding(() => { throw Object.assign(new Error("fixture unavailable"), { code: "retryable" }); }),
      } });
      state.storage.sql.exec(`INSERT INTO session_state (
        singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
        public_origin, runtime_profile, last_active
      ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team', 1,
        'https://nanocodex.example/', 'managed', ?)`, crypto.randomUUID(), Date.now());
      for (let index = 0; index < 96; index++) {
        expect((await session.fetch(new Request(`https://session.internal/turns/before-${index}/cancel`, { method: "POST" }))).status).toBe(202);
      }
      expect((await session.fetch(new Request("https://session.internal/turns/before-0/cancel", { method: "POST" }))).status).toBe(202);
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_turn_cancel_intents").one().count).toBe(96);
      const admitted = await session.fetch(new Request("https://session.internal/turns", {
        method: "POST", body: JSON.stringify({ id: "before-95", input: "never execute uncancelled" }),
      }));
      expect(admitted.status).toBe(202);
      expect(await admitted.json()).toMatchObject({ turn_id: "before-95", state: "cancelling" });
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_turn_cancel_intents").one().count).toBe(95);
      expect(state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_turns WHERE id = 'before-95'").one().state).toBe("cancelling");
      state.storage.sql.exec("UPDATE managed_turns SET state = 'cancelled', retry_at = NULL WHERE id = 'before-95'");
      await state.storage.deleteAlarm();
    });
  });

  it("retries a failed cold cancellation while archival remains in durable backoff", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      Object.defineProperty(session, "env", { value: {
        ...runtimeEnv,
        NANOCODEX: admissionBinding(() => {
          throw Object.assign(new Error("fixture runtime temporarily unavailable"), { code: "retryable" });
        }),
      } });
      const now = Date.now();
      state.storage.sql.exec(
        `INSERT INTO session_state (
           singleton, session_id, owner_id, organization_id, team_id,
           authorization_epoch, public_origin, runtime_profile, last_active
         ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team',
                   1, 'https://nanocodex.example/', 'managed', ?)`,
        crypto.randomUUID(), now,
      );
      state.storage.sql.exec(
        `INSERT INTO managed_turns (
           id, request_key, request_hash, input_json, authorization_json, state,
           accepted_cursor, may_have_inner_operation, attempt_count, retry_at,
           created_at, accepted_at, updated_at
         ) VALUES ('cancel-retry', 'cancel-retry', 'hash', '"fixture"', '{"capabilities":[]}',
                   'cancelling', 0, 1, 48, ?, ?, ?, ?)`,
        now - 1, now - 6 * 60 * 60_000, now - 6 * 60 * 60_000, now - 60_000,
      );
      // A real oversized local tail needs archival, but an unavailable bucket
      // must not turn its maintenance alarm into a barrier for cancellation.
      const log = new DurableEventLog<{ type: string; text: string }>(state.storage);
      for (let i = 0; i < 513; i++) log.append({ type: "fixture", text: "x".repeat(32_768) });
      const maintenance = new ArchiveMaintenance(state.storage);
      await expect(maintenance.start(async () => { throw new Error("bucket unavailable"); }))
        .rejects.toThrow("bucket unavailable");
      const archiveRetryAt = maintenance.nextAttemptAt();
      const row = () => state.storage.sql.exec<{
        state: string; attempt_count: number; retry_at: number;
      }>("SELECT state, attempt_count, retry_at FROM managed_turns WHERE id = 'cancel-retry'").one();
      try {
        for (const attempt of [49, 50]) {
          await session.alarm();
          const deadline = Date.now() + 3_000;
          while (row().attempt_count < attempt && Date.now() < deadline) {
            await new Promise((resolve) => setTimeout(resolve, 10));
          }
          expect(row()).toMatchObject({ state: "cancelling", attempt_count: attempt });
          expect(maintenance.nextAttemptAt()).toBe(archiveRetryAt);
          expect(state.storage.sql.exec<{ archived_events: number }>(
            "SELECT archived_events FROM managed_event_archive_state WHERE singleton = 1",
          ).one().archived_events).toBe(0);
          expect(row().retry_at).toBeGreaterThan(Date.now() + 59_000);
          const alarm = await state.storage.getAlarm();
          expect(alarm).not.toBeNull();
          expect(alarm).toBeLessThanOrEqual(row().retry_at);
          state.storage.sql.exec("UPDATE managed_turns SET retry_at = ? WHERE id = 'cancel-retry'", Date.now() - 1);
        }
      } finally {
        state.storage.sql.exec("UPDATE managed_turns SET state = 'cancelled', retry_at = NULL WHERE id = 'cancel-retry'");
        log.clear();
        await state.storage.deleteAlarm();
      }
    });
  });

  it("stops cold cancellation retries after permanent restore failure", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      const message = "durability state at revision 353 is invalid: EOF while parsing a value at line 1 column 0";
      let attempts = 0;
      Object.defineProperty(session, "env", { value: {
        ...runtimeEnv,
        NANOCODEX: admissionBinding(() => {
          attempts++;
          throw Object.assign(new Error(message), { code: "failed" });
        }),
      } });
      const now = Date.now();
      state.storage.sql.exec(`INSERT INTO session_state (
        singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
        public_origin, runtime_profile, last_active
      ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team', 1,
        'https://nanocodex.example/', 'managed', ?)`, crypto.randomUUID(), now);
      // Reproduce an old cancellation already trapped in the one-minute loop.
      state.storage.sql.exec(`INSERT INTO managed_turns (
        id, request_hash, input_json, authorization_json, state, accepted_cursor,
        may_have_inner_operation, attempt_count, retry_at, created_at, accepted_at, updated_at
      ) VALUES ('corrupt', 'hash', '"fixture"', '{"capabilities":[]}', 'cancelling',
        0, 1, 10000, ?, ?, ?, ?)`, now - 1, now - 86400_000, now - 86400_000, now - 60_000);
      const row = () => state.storage.sql.exec<{
        state: string; retry_at: number | null; terminal_cursor: number | null;
        terminal_json: string | null; attempt_count: number;
      }>("SELECT state, retry_at, terminal_cursor, terminal_json, attempt_count FROM managed_turns WHERE id = 'corrupt'").one();
      try {
        await session.alarm();
        const deadline = Date.now() + 3_000;
        while (row().state === "cancelling" && Date.now() < deadline) {
          await new Promise(resolve => setTimeout(resolve, 10));
        }
        const failed = row();
        expect(failed).toMatchObject({ state: "failed", retry_at: null, attempt_count: 10000 });
        expect(failed.terminal_cursor).not.toBeNull();
        const receipt = await session.fetch(new Request("https://session.internal/turns/corrupt"));
        expect(await receipt.json()).toMatchObject({
          state: "failed", error: message, terminal: { type: "turn_failed", id: "corrupt", error: message },
        });
        const status = await session.fetch(new Request("https://session.internal/state"));
        expect(await status.json()).toMatchObject({ active_turns: [] });
        const attempted = attempts;
        expect(attempted).toBeGreaterThan(0);
        // Later alarms and repeated Stop must not restart or append failures.
        await session.alarm();
        await session.fetch(new Request("https://session.internal/turns/corrupt/cancel", { method: "POST" }));
        await session.alarm();
        expect(row()).toEqual(failed);
        expect(attempts).toBe(attempted);
      } finally {
        state.storage.sql.exec("UPDATE managed_turns SET state = 'failed', retry_at = NULL WHERE id = 'corrupt'");
        await state.storage.deleteAlarm();
      }
    });
  });

  for (const [prior, cancelling] of (["failed", "completed", "cancelled", "missing-dispatch"] as const)
    .flatMap(prior => [false, true].map(cancelling => [prior, cancelling] as const))) {
    it(`reconciles a Rust pending identity against a ${prior} managed projection while cancelling=${cancelling}`, async () => {
      const sessions = (env as unknown as {
        NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
      }).NANOCODEX_SESSIONS;
      await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
        const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
        // Inject the typed Rust/Worker protocol at admission, before provider
        // dispatch. The real Durable Object must reconcile its SQLite inbox.
        Object.defineProperty(session, "env", { value: {
          ...runtimeEnv,
          NANOCODEX: admissionBinding(() => {
            throw Object.assign(new Error("older durable operation is unfinished"), {
              code: "retryable", blockedBy: "older",
            });
          }),
        } });
        const now = Date.now();
        state.storage.sql.exec(
          `INSERT INTO session_state (
             singleton, session_id, owner_id, organization_id, team_id,
             authorization_epoch, public_origin, runtime_profile, completed_turns, last_active
           ) VALUES (1, ?, 'fixture-owner', 'fixture-organization', 'fixture-team',
                     1, 'https://nanocodex.example/', 'managed', ?, ?)`,
          crypto.randomUUID(), prior === "completed" ? 1 : 0, now,
        );
        state.storage.sql.exec(
          `INSERT INTO managed_turns (
             id, request_key, request_hash, input_json, authorization_json,
             state, accepted_cursor, terminal_json, terminal_cursor,
             dispatch_input_chunks, may_have_inner_operation, attempt_count,
             created_at, accepted_at, updated_at
           ) VALUES ('older', 'older', 'hash', ?, ?, ?, 0, '{}', 1, ?, 1, 0, ?, ?, ?)`,
          JSON.stringify("original input"), JSON.stringify({ capabilities: [] }),
          prior === "missing-dispatch" ? "failed" : prior,
          prior === "missing-dispatch" ? null : 1, now - 1, now - 1, now - 1,
        );
        if (prior !== "missing-dispatch") {
          state.storage.sql.exec(
            "INSERT INTO managed_turn_dispatch_chunks VALUES ('older', 0, ?)",
            JSON.stringify("original input"),
          );
        }
        if (cancelling) {
          expect((await session.fetch(new Request("https://session.internal/turns/later/cancel", { method: "POST" }))).status).toBe(202);
        }
        const response = await session.fetch(new Request("https://session.internal/turns", {
          method: "POST", body: JSON.stringify({ id: "later", input: "follow on" }),
        }));
        expect(response.status).toBe(202);
        const deadline = Date.now() + 3_000;
        while (state.storage.sql.exec<{ retry_at: number | null }>(
          "SELECT retry_at FROM managed_turns WHERE id = 'later'",
        ).one().retry_at === null && Date.now() < deadline) {
          await new Promise((resolve) => setTimeout(resolve, 10));
        }
        const rows = state.storage.sql.exec<{
          id: string; state: string; terminal_json: string | null; terminal_cursor: number | null;
          retry_at: number | null;
        }>("SELECT id, state, terminal_json, terminal_cursor, retry_at FROM managed_turns ORDER BY created_at").toArray();
        expect(rows[1]).toMatchObject({ id: "later", state: cancelling ? "cancelling" : "accepted" });
        expect(rows[1]!.retry_at).not.toBeNull();
        expect(rows[0]).toMatchObject(prior === "missing-dispatch" ? {
          id: "older", state: "failed", terminal_json: "{}", terminal_cursor: 1,
        } : {
          id: "older", state: prior === "cancelled" ? "cancelling" : "accepted",
          terminal_json: null, terminal_cursor: null,
        });
        expect(state.storage.sql.exec<{ completed_turns: number }>(
          "SELECT completed_turns FROM session_state WHERE singleton = 1",
        ).one().completed_turns).toBe(0);
      });
    });
  }

  for (const connected of [false, true]) {
    it(`retains a recovery alarm during stalled admission with connected=${connected}`, async () => {
      const sessions = (env as unknown as {
        NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
      }).NANOCODEX_SESSIONS;
      await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
        let bindingCalls = 0;
        const binding = Promise.withResolvers<Response>();
        const entered = Promise.withResolvers<void>();
        const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
        Object.defineProperty(session, "env", { value: {
          ...runtimeEnv,
          NANOCODEX: admissionBinding(() => {
            bindingCalls++;
            entered.resolve();
            return binding.promise;
          }),
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
          // Simulate receipt backlog retained after an interrupted archive
          // write. The live admission must not make its alarm skip archival.
          state.storage.transactionSync(() => {
            for (let index = 0; index < 513; index += 1) {
              const id = `archived-${index}`;
              state.storage.sql.exec(
                `INSERT INTO managed_turns (
                   id, request_hash, input_json, authorization_json, state, accepted_cursor,
                   terminal_json, terminal_cursor, created_at, accepted_at, updated_at
                 ) VALUES (?, ?, '"fixture"', '{"capabilities":[]}', 'cancelled', 1, ?, 2, ?, ?, ?)`,
                id, "a".repeat(64), JSON.stringify({ type: "turn_cancelled", id }), now, now, now,
              );
              state.storage.sql.exec(
                `INSERT INTO managed_realtime_operations (
                   voice_session_id, operation_id, kind, request_hash, state,
                   response_json, created_at, updated_at
                 ) VALUES ('fixture-voice', ?, 'stop', ?, 'completed', '{}', ?, ?)`,
                id, "b".repeat(64), now, now,
              );
            }
          });
          await session.alarm();
          for (const table of ["managed_turn_archive_state", "managed_realtime_archive_state"]) {
            await expect.poll(() => state.storage.sql.exec<{ archived_receipts: number }>(
              `SELECT archived_receipts FROM ${table} WHERE singleton = 1`,
            ).one().archived_receipts).toBe(1);
          }
          expect(state.storage.sql.exec<{ state: string }>(
            "SELECT state FROM managed_turns WHERE id = 'stalled'",
          ).one().state).toBe("accepted");
          const alarm = await state.storage.getAlarm();
          expect(alarm).toBeGreaterThanOrEqual(Date.now() + 59_000);
          expect(alarm).toBeLessThanOrEqual(Date.now() + 60_000);
          // Model three one-minute recovery leases passing while the same
          // admitted owner is awaiting I/O. A lease is a reconstruction wakeup,
          // not a timeout authorizing a second live admission.
          const clock = vi.spyOn(Date, "now");
          try {
            for (let lease = 1; lease <= 3; lease++) {
              clock.mockReturnValue(now + lease * 60_000);
              await session.alarm();
              await Promise.resolve();
              expect(bindingCalls).toBe(1);
              expect(state.storage.sql.exec<{ state: string; attempt_count: number; retry_at: number | null }>(
                "SELECT state, attempt_count, retry_at FROM managed_turns WHERE id = 'stalled'",
              ).one()).toEqual({ state: "accepted", attempt_count: 0, retry_at: null });
              expect(await state.storage.getAlarm()).toBe(now + (lease + 1) * 60_000);
            }
          } finally { clock.mockRestore(); }

        } finally {
          binding.resolve(new Response(null, { status: 204 }));
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
        agent_loaded: boolean;
      }>();
      expect(retained.agent_loaded).toBe(false);
      expect(retained.active_turns).toEqual([
        ...Array.from({ length: FIXTURE_UNFINISHED_TURNS }, (_, index) => `fixture-${index}`),
        "accepted-beyond-sixteen",
      ]);
      expect(retained).not.toHaveProperty("active_turn_details");
      const accepted = await session.fetch(new Request("https://session.internal/turns/accepted-beyond-sixteen"));
      expect(await accepted.json()).toMatchObject({ turn_id: "accepted-beyond-sixteen", input: "queued prompt" });

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


describe("managed steer withdrawal admission", () => {
  it("rejects malformed identities before looking up or recovering a turn", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session) => {
      for (const body of ["null", "[]", "{}", JSON.stringify({ message_id: "../invalid" })]) {
        const response = await session.fetch(new Request("https://session.internal/turns/absent/withdraw-steer", { method: "POST", body }));
        expect(response.status).toBe(400);
        expect(await response.json()).toMatchObject({ error: "invalid_request" });
      }
      const missing = await session.fetch(new Request("https://session.internal/turns/absent/withdraw-steer", {
        method: "POST", body: JSON.stringify({ message_id: "pending" }),
      }));
      expect(missing.status).toBe(404);
    });
  });
});

it("keeps a real managed automatic compaction owned across three recovery alarms", async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    const entered = Promise.withResolvers<void>();
    let finish!: () => void;
    let requests = 0;
    class ModelSocket extends EventTarget {
      readyState = 1;
      accept() {}
      close() { this.readyState = 3; }
      emit(value: unknown) { this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(value) })); }
      send(data: string) {
        const request = JSON.parse(data);
        requests++;
        if (requests === 2) {
          expect(JSON.stringify(request)).toContain("compaction");
          finish = () => {
            this.emit({ type: "response.output_item.done", item: { id: "summary", type: "compaction", encrypted_content: "opaque-summary" } });
            this.emit({ type: "response.completed", response: { id: "compact", status: "completed", output: [], usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 } } });
          };
          entered.resolve();
          return;
        }
        queueMicrotask(() => this.emit({ type: "response.completed", response: {
          id: `response-${requests}`, status: "completed", end_turn: true,
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Hello" }] }],
          usage: { input_tokens: requests === 1 ? 300_000 : 100, output_tokens: 1, total_tokens: requests === 1 ? 300_001 : 101 },
        } }));
      }
    }
    const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { value: { ...runtimeEnv,
      NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => Response.json({}) }) },
      NANOCODEX: { async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        if (new Headers(init?.headers).get("upgrade") === "websocket" || request.headers.get("upgrade") === "websocket")
          return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
        return Response.json({ tools: [], machines: [], connections: [] });
      } },
    } });
    const now = Date.now();
    state.storage.sql.exec(`INSERT INTO session_state (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active)
      VALUES (1, ?, 'fixture-owner', 'fixture-org', 'fixture-team', 1, 'https://nanocodex.example/', 'managed', ?)`, crypto.randomUUID(), now);
    state.storage.sql.exec("INSERT INTO managed_configuration VALUES (1, ?)", JSON.stringify({ tools: [], environment: { files: [], skills: [], setup_commands: [], network: { access: "disabled" } } }));
    state.storage.sql.exec("UPDATE managed_agent_settings SET model = 'gpt-6-sol', thinking = 'low'");
    const seed = (id: string) => {
      state.storage.sql.exec(`INSERT INTO managed_turns (id, request_hash, input_json, authorization_json, state, accepted_cursor, dispatch_input_chunks, may_have_inner_operation, attempt_count, created_at, accepted_at, updated_at)
        VALUES (?, 'hash', '"fixture"', '{"capabilities":[]}', 'accepted', 0, 1, 0, 0, ?, ?, ?)`, id, now, now, now);
      state.storage.sql.exec("INSERT INTO managed_turn_dispatch_chunks VALUES (?, 0, '\"fixture\"')", id);
    };
    seed("first");
    await session.alarm();
    await expect.poll(() => state.storage.sql.exec("SELECT state, error FROM managed_turns WHERE id='first'").one()).toEqual({ state: "completed", error: null });
    seed("second");
    await session.alarm();
    await entered.promise;
    const clock = vi.spyOn(Date, "now");
    try {
      for (let lease = 1; lease <= 3; lease++) {
        clock.mockReturnValue(now + lease * 60_000);
        await session.alarm();
        expect(requests).toBe(2);
        expect(await state.storage.getAlarm()).toBe(now + (lease + 1) * 60_000);
        expect(state.storage.sql.exec<{ count: number }>(
          "SELECT COUNT(*) AS count FROM managed_events WHERE turn_id = 'second' AND json_extract(message_json, '$.type') = 'turn_retryable'",
        ).one().count).toBe(0);
        expect(state.storage.sql.exec<{ state: string; attempt_count: number; retry_at: number | null }>("SELECT state, attempt_count, retry_at FROM managed_turns WHERE id='second'").one())
          .toEqual({ state: "accepted", attempt_count: 0, retry_at: null });
      }
    } finally { clock.mockRestore(); finish(); }
    await expect.poll(() => state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_turns WHERE id='second'").one().state).toBe("completed");
    expect(requests).toBe(3);
    await state.storage.deleteAlarm();
  });
}, 30_000);
