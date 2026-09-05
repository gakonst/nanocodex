import { env, runInDurableObject, runDurableObjectAlarm, createExecutionContext } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import type { Principal } from "../src/account-auth";
import { CronTriggers, nextCronRun, parseCronTrigger } from "../src/cron-triggers";

const now = Date.parse("2026-09-05T12:00:00Z");
const config = { cron: "* * * * *", input: "Check the system" };
const request = (id: string, body: unknown = config) => new Request(`https://session.internal/triggers/${id}`, {
  method: "PUT", body: JSON.stringify(body),
});
const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;

function initialize(state: DurableObjectState) {
  state.storage.sql.exec(`INSERT INTO session_state (
    singleton, session_id, owner_id, organization_id, team_id,
    authorization_epoch, public_origin, runtime_profile, last_active
  ) VALUES (1, ?, 'owner', 'org', 'team', 1, 'https://nanocodex.example', 'managed', ?)`,
  crypto.randomUUID(), Date.now());
}

function retainBusyTurn(state: DurableObjectState) {
  state.storage.sql.exec(`INSERT INTO managed_turns (
    id, request_hash, input_json, authorization_json, state, accepted_cursor,
    created_at, accepted_at, updated_at, retry_at
  ) VALUES ('busy', 'hash', '"busy"', '{"capabilities":[]}', 'accepted', 1, ?, ?, ?, ?)`,
  Date.now(), Date.now(), Date.now(), Date.now() + 60_000);
}

describe("cron policy", () => {
  it("uses five fields, UTC by default, and skips to the next minute", () => {
    expect(parseCronTrigger(config, now)).toEqual({ ...config, timezone: "UTC", enabled: true });
    expect(nextCronRun("*/15 * * * *", "UTC", now + 1)).toBe(now + 15 * 60_000);
    expect(nextCronRun("0 7 * * MON-FRI", "Europe/Athens", now)).toBe(Date.parse("2026-09-07T04:00:00Z"));
    expect(nextCronRun("0 0 29 2 *", "UTC", now)).toBe(Date.parse("2028-02-29T00:00:00Z"));
  });

  it("keeps the local morning hour across both DST transitions", () => {
    expect(nextCronRun("0 7 * * *", "Europe/Athens", Date.parse("2026-03-28T06:00:00Z")))
      .toBe(Date.parse("2026-03-29T04:00:00Z"));
    expect(nextCronRun("0 7 * * *", "Europe/Athens", Date.parse("2026-10-24T06:00:00Z")))
      .toBe(Date.parse("2026-10-25T05:00:00Z"));
  });

  it.each([
    { cron: "* * * * * *" }, { cron: "@daily" }, { cron: "H * * * *" },
    { cron: "61 * * * *" }, { cron: "0 0 31 2 *" }, { timezone: "Mars/Olympus" },
    { input: " " }, { input: "x".repeat(65_537) }, { enabled: "yes" }, { extra: true },
  ])("rejects invalid or unbounded configuration %#", (patch) => {
    expect(() => parseCronTrigger({ ...config, ...patch }, now)).toThrow();
  });
});

describe("cron Durable Object protocol", () => {
  it("persists an idle wakeup, idempotently replaces, pauses, resumes, and deletes", async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (session, state) => {
      initialize(state);
      const created = await session.fetch(request("daily"));
      expect(created.status).toBe(201);
      const first = await created.json<{ next_run_at: number }>();
      expect(await state.storage.getAlarm()).toBe(first.next_run_at);
      const retried = await session.fetch(request("daily"));
      expect(retried.status).toBe(200);
      expect(await retried.json()).toEqual(first);
      expect(JSON.stringify(first)).not.toMatch(/authorization|revision|request_hash/);
      const paused = await session.fetch(request("daily", { ...config, enabled: false }));
      expect(await paused.json()).toMatchObject({ enabled: false, next_run_at: null });
      expect(await state.storage.getAlarm()).toBeNull();
      await session.fetch(request("daily"));
      expect(await state.storage.getAlarm()).not.toBeNull();
      expect((await session.fetch(new Request("https://session.internal/durability/export", { method: "POST" }))).status).toBe(409);
      for (let i = 0; i < 2; i++) {
        expect((await session.fetch(new Request("https://session.internal/triggers/daily", { method: "DELETE" }))).status).toBe(204);
      }
      expect(await state.storage.getAlarm()).toBeNull();
      const listed = await session.fetch(new Request("https://session.internal/triggers"));
      expect(await listed.json()).toEqual({ data: [] });
    });
  });

  it("coalesces missed ticks, admits one durable turn, and never duplicates an alarm", async () => {
    const stub = sessions().getByName(crypto.randomUUID());
    await runInDurableObject(stub, async (session, state) => {
      initialize(state);
      // Keep execution at the existing durable retry boundary. No model credentials are needed.
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      Object.defineProperty(session, "env", { value: {
        ...runtimeEnv, NANOCODEX_ACCOUNT_TOOLS: { getByName: () => {
          throw Object.assign(new Error("fixture unavailable"), { code: "retryable" });
        } },
      } });
      await session.fetch(request("daily"));
      state.storage.sql.exec("UPDATE managed_cron_triggers SET next_run_at = ?", Date.now() - 86_400_000);
    });
    expect(await runDurableObjectAlarm(stub)).toBe(true);
    await runInDurableObject(stub, async (session, state) => {
      const row = new CronTriggers(state.storage).get("daily")!;
      expect(row.last_turn_id).toMatch(/^cron:/);
      expect(row.next_run_at).toBeGreaterThan(Date.now());
      const receipt = await session.fetch(new Request(`https://session.internal/turns/${row.last_turn_id}`));
      expect(receipt.status).toBe(200);
      expect(await receipt.json()).toMatchObject({ turn_id: row.last_turn_id });
      await session.alarm();
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_turns").one().count).toBe(1);
      expect(new CronTriggers(state.storage).get("daily")!.last_turn_id).toBe(row.last_turn_id);
    });
  });

  it("skips a busy agent without growing its inbox", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      initialize(state);
      retainBusyTurn(state);
      await session.fetch(request("daily"));
      state.storage.sql.exec("UPDATE managed_cron_triggers SET next_run_at = ?", Date.now() - 1);
      await session.alarm();
      const row = new CronTriggers(state.storage).get("daily")!;
      expect(row.last_turn_id).toBeNull();
      expect(row.last_skipped_at).not.toBeNull();
      expect(row.next_run_at).toBeGreaterThan(Date.now());
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_turns").one().count).toBe(1);
    });
  });

  it("rolls back advancement with turn admission and fences edits, pause, and recreation", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      initialize(state);
      const triggers = new CronTriggers(state.storage);
      await session.fetch(request("daily"));
      const original = triggers.get("daily")!;
      expect(() => state.storage.transactionSync(() => {
        expect(triggers.advance(original, Date.now(), Date.now() + 60_000, "turn")).toBe(true);
        throw new Error("admission failed");
      })).toThrow("admission failed");
      expect(triggers.get("daily")).toEqual(original);
      for (const replacement of [{ ...config, input: "new" }, { ...config, enabled: false }, config]) {
        await session.fetch(request("daily", replacement));
        expect(triggers.advance(original, Date.now(), Date.now() + 60_000, "stale")).toBe(false);
      }
      triggers.delete("daily");
      await session.fetch(request("daily"));
      expect(triggers.advance(original, Date.now(), Date.now() + 60_000, "stale")).toBe(false);
    });
  });

  it("retains retry deadlines across reconstruction and clears them on replacement", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      initialize(state);
      await session.fetch(request("daily"));
      state.storage.sql.exec("UPDATE managed_cron_triggers SET next_run_at = ?", Date.now() - 60_000);
      const triggers = new CronTriggers(state.storage);
      const due = triggers.get("daily")!;
      const retryAt = Date.now() + 60_000;
      triggers.retry(due, retryAt);
      const restored = new CronTriggers(state.storage);
      expect(restored.nextAlarm()).toBe(retryAt);
      expect(restored.due(Date.now())).toEqual([]);
      expect(restored.due(retryAt)).toHaveLength(1);
      await session.fetch(request("daily", { ...config, input: "replacement" }));
      expect(restored.get("daily")!.retry_at).toBeNull();
      restored.retry(due, retryAt + 60_000);
      expect(restored.get("daily")!.retry_at).toBeNull();
    });
  });

  it("enforces ownership assertions and the per-agent limit", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      initialize(state);
      const wrongOwner = request("daily");
      wrongOwner.headers.set("x-nanocodex-owner-id", "other");
      expect((await session.fetch(wrongOwner)).status).toBe(404);
      for (let index = 0; index < 32; index++) expect((await session.fetch(request(`t${index}`))).status).toBe(201);
      expect((await session.fetch(request("overflow"))).status).toBe(429);
      expect((await session.fetch(request("t0"))).status).toBe(200);
      expect((await session.fetch(request("bad", { ...config, timezone: "invalid" }))).status).toBe(400);
    });
  });
});


describe("cron HTTP routes", () => {
  it("applies capability, origin, and ownership checks before routing to the real session", async () => {
    const id = "0198d3f0-8844-7000-8000-000000000001";
    const owner = "11111111-1111-4111-8111-111111111111";
    const org = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const team = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const principal: Principal = {
      kind: "api_key", userId: owner, organizationId: org, teamId: team,
      role: "owner", subjectId: `user:${owner}`, credentialId: "test",
      authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
    };
    await runInDurableObject(sessions().getByName(id), async (_session, state) => {
      initialize(state);
      state.storage.sql.exec("UPDATE session_state SET session_id = ?, owner_id = ?, organization_id = ?, team_id = ?", id, owner, org, team);
    });
    const call = (method: string, actor = principal, origin?: string, suffix = "/daily") => worker.fetch(
      new Request(`https://nanocodex.example/v1/agents/${id}/triggers${suffix}`, {
        method, ...(method === "PUT" ? { body: JSON.stringify(config) } : {}),
        headers: origin ? { origin } : {},
      }), env as Parameters<typeof worker.fetch>[1], createExecutionContext(), actor,
    );
    expect((await call("PUT", { ...principal, capabilities: ["agents:write"] })).status).toBe(403);
    expect((await call("GET", { ...principal, capabilities: ["agents:write"] })).status).toBe(403);
    expect((await call("PUT", { ...principal, kind: "account_session" }, "https://evil.example")).status).toBe(403);
    expect((await call("PUT", { ...principal, userId: "22222222-2222-4222-8222-222222222222" })).status).toBe(404);
    expect((await call("PUT", { ...principal, connectGrant: { grantId: `0x${"a".repeat(64)}`, connectors: ["chatgpt"], mcpIds: [] } })).status).toBe(403);
    expect((await call("PUT")).status).toBe(201);
    expect(await (await call("GET")).json()).toMatchObject({ id: "daily", enabled: true });
    expect((await call("GET", principal, undefined, "")).status).toBe(200);
    expect((await call("POST")).status).toBe(405);
    expect((await call("DELETE")).status).toBe(204);
    expect((await call("GET")).status).toBe(404);
  });
});
