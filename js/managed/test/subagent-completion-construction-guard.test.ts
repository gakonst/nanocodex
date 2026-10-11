import { env, runInDurableObject } from "cloudflare:test";
import { afterEach, expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { recordSubagentCompletion } from "../src/index";
import { attachAgent, type AccountAuthEnv } from "../src/account-auth";

const GUARD_KEY = "managed.runtime_construction_guard";
const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;

async function initialize(session: DurableAgentSession, state: DurableObjectState, owner = "owner") {
  // The first real request creates the schema; this ownerless one is refused.
  await session.fetch(new Request("https://session.internal/sites"));
  const id = crypto.randomUUID();
  state.storage.sql.exec(`INSERT INTO session_state (
    singleton, session_id, owner_id, organization_id, team_id,
    authorization_epoch, public_origin, runtime_profile, last_active
  ) VALUES (1, ?, ?, 'org', 'team', 1, 'https://nanocodex.example', 'managed', ?)`,
  id, owner, Date.now());
  await attachAgent(env as unknown as AccountAuthEnv, owner, id, undefined, false);
}

/** Runtime construction starts with the credential broker; count its calls. */
function observeConstruction(session: DurableAgentSession) {
  const broker = vi.fn(async () => new Response(null, { status: 503 }));
  const discovery = vi.fn(async () => new Response(null, { status: 503 }));
  const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
  Object.defineProperty(session, "env", { value: { ...runtimeEnv, NANOCODEX: { fetch: broker },
    NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: discovery }) } } });
  return broker;
}

const outboxRow = (state: DurableObjectState) => state.storage.sql.exec<{ attempts: number; next_at: number; settled: string | null }>(
  "SELECT attempts, next_at, settled FROM managed_subagent_completions WHERE session_id = 'child-a' AND revision = 1").one();

afterEach(() => vi.restoreAllMocks());

// After a runtime construction died with its isolate (#980), a due outbox row
// must not rebuild the runtime from an alarm: it stays undecided and backs off.
it("a guarded object's outbox alarm does not construct a runtime and keeps the completion pending", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
    await initialize(session, state);
    expect(recordSubagentCompletion(state.storage, "child-a", 1)).toBe(true);
    // A construction owned by an isolate that was reset, after an earlier strike.
    const guard = { token: "reset-isolate", strikes: 1, at: Date.now() };
    state.storage.kv.put(GUARD_KEY, guard);
    const broker = observeConstruction(session);
    const warn = vi.spyOn(console, "warn");
    const before = Date.now();

    await session.alarm();

    expect(broker).not.toHaveBeenCalled();
    expect(state.storage.kv.get(GUARD_KEY)).toEqual(guard);
    const row = outboxRow(state);
    expect(row.settled).toBeNull();
    expect(row.attempts).toBe(1);
    expect(row.next_at).toBeGreaterThanOrEqual(before + 2_000);
    const logged = warn.mock.calls.map(([entry]) => entry as { type?: string; action?: string } | undefined);
    expect(logged.filter(entry => entry?.type === "managed.subagent_completion" && entry.action === "alarm_drain_paused")).toHaveLength(1);
    expect(logged.filter(entry => entry?.action === "alarm_drain_failed")).toEqual([]);
    await state.storage.deleteAlarm();
  });
});

// Without strikes the outbox alarm still rebuilds the runtime to deliver.
it("an unguarded object's outbox alarm still constructs a runtime to deliver", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
    await initialize(session, state);
    expect(recordSubagentCompletion(state.storage, "child-a", 1)).toBe(true);
    expect(state.storage.kv.get(GUARD_KEY)).toBeUndefined();
    const broker = observeConstruction(session);
    const warn = vi.spyOn(console, "warn");

    await session.alarm();

    expect(broker).toHaveBeenCalled();
    const logged = warn.mock.calls.map(([entry]) => entry as { type?: string; action?: string } | undefined);
    expect(logged.filter(entry => entry?.action === "alarm_drain_paused")).toEqual([]);
    // The rebuild failed here (the broker is unavailable), so the row backs off undecided.
    expect(outboxRow(state).settled).toBeNull();
    expect(outboxRow(state).attempts).toBe(1);
    await state.storage.deleteAlarm();
  });
});
