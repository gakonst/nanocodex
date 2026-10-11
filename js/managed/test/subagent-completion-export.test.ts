import { env, runInDurableObject } from "cloudflare:test";
import { afterEach, expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { pendingSubagentCompletions, recordSubagentCompletion, settleSubagentCompletion } from "../src/index";
import { attachAgent, type AccountAuthEnv } from "../src/account-auth";

const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;

const exportRequest = () => new Request("https://session.internal/durability/export", { method: "POST" });

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

afterEach(() => vi.restoreAllMocks());

// The completion outbox is never exported: an undecided row would be lost with
// the source, whose parent would then never wake for that completion.
it("refuses export while a completion is undecided and leaves no outbox alarm after export", async () => {
  const stub = sessions().getByName(crypto.randomUUID());
  await runInDurableObject(stub, async (session, state) => {
    await initialize(session, state);
    expect(recordSubagentCompletion(state.storage, "child-a", 1)).toBe(true);

    const refused = await session.fetch(exportRequest());
    expect(refused.status).toBe(409);
    expect(refused.headers.get("retry-after")).toBe("2");
    expect(await refused.json()).toMatchObject({ error: "subagent_completions_pending" });
    expect(await state.storage.get("nanocodex:durability-exported")).toBeUndefined();
    expect(pendingSubagentCompletions(state.storage)).toHaveLength(1);

    // Once the parent decided it, export proceeds past the outbox gate.
    expect(settleSubagentCompletion(state.storage, "child-a", 1, "woken")).toBe(true);
    const exported = await session.fetch(exportRequest());
    expect(exported.status).not.toBe(409);
    expect(await exported.clone().text()).not.toContain("subagent_completions_pending");
    expect(await state.storage.get("nanocodex:durability-exported")).toBe(true);

    // A row left behind (for example a late announcement) is no longer owned
    // by the exported source: no alarm target, no drain, no backoff.
    expect(recordSubagentCompletion(state.storage, "child-b", 1)).toBe(true);
    const info = vi.spyOn(console, "info");
    const warn = vi.spyOn(console, "warn");
    await session.alarm();
    const logged = [...info.mock.calls, ...warn.mock.calls].map(([entry]) => entry as { type?: string; action?: string } | undefined);
    expect(logged.filter(entry => entry?.type === "managed.subagent_completion_alarm")).toEqual([]);
    expect(logged.filter(entry => entry?.type === "managed.subagent_completion" && entry.action === "alarm_drain_failed")).toEqual([]);
    expect(state.storage.sql.exec<{ attempts: number; settled: string | null }>(
      "SELECT attempts, settled FROM managed_subagent_completions WHERE session_id = 'child-b'").toArray())
      .toEqual([{ attempts: 0, settled: null }]);
  });
});
