import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { Goals } from "../src/goals";
import { GoalRuntime } from "../src/goal-runtime";

const sessions = () => (env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
}).NANOCODEX_SESSIONS;

function fixture(session: DurableAgentSession, state: DurableObjectState) {
  const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
  const thread = crypto.randomUUID(), now = Date.now();
  state.storage.sql.exec(`INSERT INTO session_state
    (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
     public_origin, runtime_profile, last_active)
    VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example/', 'managed', ?)`,
    thread, owner, organization, team, now);
  const model = vi.fn(async () => new Response("model dispatch forbidden in goal control test", { status: 503 }));
  const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
  Object.defineProperty(session, "env", { value: { ...runtimeEnv,
    NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => Response.json({}) }) },
    NANOCODEX: { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input, init);
      // Cancellation may discover tool catalogs; only provider dispatch is forbidden.
      if (new URL(request.url).pathname.endsWith("/catalog")) {
        return Response.json({ tools: [], machines: [], connections: [] });
      }
      return model();
    } },
  } });
  const seedBusy = (id: string) => {
    state.storage.sql.exec(`INSERT INTO managed_turns
      (id, request_hash, input_json, authorization_json, state, accepted_cursor,
       may_have_inner_operation, attempt_count, retry_at, created_at, accepted_at, updated_at)
      VALUES (?, 'fixture', '"unrelated retained work"', '{"capabilities":[]}',
              'accepted', 0, 0, 1, ?, ?, ?, ?)`, id, now + 3_600_000, now, now, now);
  };
  seedBusy("unrelated");
  const goals = new Goals(state.storage, () => thread);
  const runtime = new GoalRuntime(state.storage, goals);
  const post = (id: string, command: string, headers?: Record<string, string>) => session.fetch(
    new Request("https://session.internal/turns", {
      method: "POST", headers,
      body: JSON.stringify({ id, input: [{ type: "text", text: command }] }),
    }),
  );
  const row = (id: string) => state.storage.sql.exec<{ state: string; retry_at: number | null; may_have_inner_operation: number }>(
    "SELECT state, retry_at, may_have_inner_operation FROM managed_turns WHERE id=?", id,
  ).one();
  const cleanup = async () => {
    runtime.clear();
    state.storage.sql.exec("UPDATE managed_turns SET state='cancelled', retry_at=NULL WHERE state IN ('accepted','cancelling')");
    await state.storage.deleteAlarm();
  };
  const headers = {
    "x-nanocodex-owner-id": owner, "x-nanocodex-session-organization-id": organization,
    "x-nanocodex-session-team-id": team, "x-nanocodex-authorization-epoch": "1",
    "x-nanocodex-capabilities": '["agents:write","tools:use"]',
  };
  return { post, row, seedBusy, goals, runtime, model, cleanup, headers };
}

describe("goal HTTP admission", () => {
  it("completes creation and status without model dispatch, replays once, and retains continuation behind busy work", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      const f = fixture(session, state);
      try {
        const busy = f.row("unrelated");
        const created = await f.post("create", "/goal Ship  the feature");
        expect(created.status).toBe(202);
        const receipt = await created.json();
        expect(receipt).toMatchObject({ turn_id: "create", state: "completed",
          terminal: { type: "turn_completed", final_message: expect.stringContaining("Ship  the feature") } });
        const events = (id: string) => state.storage.sql.exec<{ message_json: string }>(
          "SELECT message_json FROM managed_events WHERE turn_id=? ORDER BY cursor", id,
        ).toArray().map(row => JSON.parse(row.message_json));
        const completedEvents = events("create");
        expect(completedEvents.map(event => event.type)).toEqual(["turn_accepted", "event", "turn_completed"]);
        expect(completedEvents[1]).toMatchObject({ type: "event", event: {
          protocol_version: 1, type: "run.completed", payload: { status: "completed" },
        } });
        const goal = f.goals.get();
        expect(goal).toMatchObject({ objective: "Ship  the feature", status: "active", tokensUsed: 0 });
        const pending = f.runtime.pending();
        expect(pending).toMatchObject({ turn_id: "create", goal_id: goal!.goalId, epoch: 1 });
        const replay = await f.post("create", "/goal Ship  the feature");
        expect(replay.status).toBe(200);
        expect(await replay.json()).toEqual(receipt);
        expect(events("create")).toEqual(completedEvents);
        expect(f.goals.get()).toEqual(goal);
        const conflict = await f.post("create", "/goal Different objective");
        expect(conflict.status).toBe(409);
        for (const [id, command] of [["status", "/goal status"], ["bare", "/goal"]]) {
          const response = await f.post(id, command);
          expect(response.status).toBe(202);
          expect(await response.json()).toMatchObject({ turn_id: id, state: "completed",
            terminal: { final_message: expect.stringContaining("Goal active: Ship  the feature") } });
          expect(events(id).map(event => event.event?.type ?? event.type)).toEqual(["turn_accepted", "run.completed", "turn_completed"]);
          expect(f.runtime.pending()).toEqual(pending);
        }
        expect(f.row("unrelated")).toEqual(busy);
        expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_turns WHERE id<>'unrelated'").one().n).toBe(3);
        expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_turns WHERE may_have_inner_operation=1").one().n).toBe(0);
        expect(f.model).not.toHaveBeenCalled();
      } finally { await f.cleanup(); }
    });
  });

  for (const command of ["pause", "clear"]) {
    it(`${command} cancels only goal-bound work and leaves unrelated retained work intact`, async () => {
      await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
        const f = fixture(session, state);
        try {
          expect((await f.post("create", "/goal Finish the task")).status).toBe(202);
          f.seedBusy("goal-work");
          f.runtime.bind("goal-work", 1);
          const unrelated = f.row("unrelated");
          const response = await f.post("control", `/goal ${command}`);
          expect(response.status).toBe(202);
          expect(await response.json()).toMatchObject({ turn_id: "control", state: "completed",
            terminal: { type: "turn_completed", final_message: command === "clear" ? "No goal is set." : expect.stringContaining("Goal paused:") } });
          expect(["cancelling", "cancelled"]).toContain(f.row("goal-work").state);
          expect(f.row("unrelated")).toEqual(unrelated);
          expect(f.runtime.pending()).toBeUndefined();
          if (command === "clear") expect(f.goals.get()).toBeNull();
          else expect(f.goals.get()).toMatchObject({ status: "paused" });
          expect(f.model).not.toHaveBeenCalled();
        } finally { await f.cleanup(); }
      });
    });
  }

  it("rejects Connect-grant goal controls before creating a goal or durable turn", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      const f = fixture(session, state);
      try {
        const response = await f.post("denied", "/goal Escalate privileges", {
          ...f.headers, "x-nanocodex-connect-grant-id": `0x${"a".repeat(64)}`,
          "x-nanocodex-connect-connectors": '["chatgpt"]', "x-nanocodex-connect-mcp-ids": "[]",
        });
        expect(response.status).toBe(403);
        expect(await response.json()).toMatchObject({ error: "forbidden", message: "goal controls require full account authority" });
        expect(f.goals.get()).toBeNull();
        expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_turns WHERE id='denied'").one().n).toBe(0);
        expect(f.model).not.toHaveBeenCalled();
      } finally { await f.cleanup(); }
    });
  });
});
