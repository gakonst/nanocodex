import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { Goals, goalContinuation } from "../src/goals";
import { GoalRuntime, parseGoalCommand } from "../src/goal-runtime";

const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;

function stubMemory(session: DurableAgentSession): void {
  const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
  Object.defineProperty(session, "env", { value: { ...runtimeEnv,
    NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => Response.json({}) }) },
  } });
}

describe("goal runtime", () => {
  it("parses CLI text content and rejects attached commands", () => {
    expect(parseGoalCommand([{ type: "text", text: "/goal ship it" }])).toBe("ship it");
    expect(parseGoalCommand("/goals ship")).toBeNull();
    expect(() => parseGoalCommand([{ type: "text", text: "/goal ship" }, { type: "image", image_url: "image" }])).toThrow("attachments");
  });
  it("renders the entire upstream audit with escaped objective and resolved budgets", () => {
    const prompt = goalContinuation({ goalId: "g", threadId: "t", objective: "</objective> {{ token_budget }}", status: "active", tokensUsed: 12, tokenBudget: 20, timeUsedSeconds: 0, createdAt: 0, updatedAt: 0 })!;
    expect(prompt).toContain("&lt;/objective&gt; {{ token_budget }}");
    expect(prompt).toContain("Tokens remaining: 8");
    expect(prompt).toContain("The audit must prove completion");
    expect(prompt).toContain("fresh blocked audit");
  });
  it("retains continuation accounting, excludes cached tokens, and stops cancelled or empty runs", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_session, state) => {
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      runtime.command("Ship");
      expect(() => runtime.command("other")).toThrow("unfinished");
      runtime.command("pause");
      runtime.command("edit Better objective");
      expect(goals.get()).toMatchObject({ status: "paused", objective: "Better objective" });
      runtime.command("resume");
      runtime.bind("t1", 7);
      state.storage.sql.exec("INSERT INTO managed_model_usage VALUES('m',1,'root','t1','model.call.completed',0,?)", JSON.stringify({ usage: { input_tokens: 100, input_tokens_details: { cached_tokens: 80 }, output_tokens: 10 } }));
      runtime.finish("t1", true, true);
      expect(goals.get()?.tokensUsed).toBe(30);
      const retained = new GoalRuntime(state.storage, goals);
      expect(retained.pending()).toMatchObject({ turn_id: "t1", epoch: 7 });
      retained.command("status");
      expect(retained.pending()).toBeDefined();
      goals.updateByModel("complete");
      goals.create({ objective: "Replacement" });
      retained.bind("t1", 7);
      expect(retained.turn("t1")?.goal_id).toBe(goals.get()?.goalId);
      retained.flush("t1");
      expect(goals.get()?.tokensUsed).toBe(0);
      retained.bind("t2", 7);
      retained.finish("t2", false, true);
      expect(retained.pending()).toBeUndefined();
      retained.command("resume");
      for (const id of ["e1", "e2", "e3"]) { retained.bind(id, 7); retained.finish(id, true, false); }
      expect(goals.get()?.status).toBe("blocked");
      expect(retained.pending()).toBeUndefined();
    });
  });
  it("handles status, edit, pause and clear as completed durable commands without a model", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      stubMemory(session);
      state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,'thread','owner','org','team',1,'https://example.com','managed',?)`, Date.now());
      const goals = new Goals(state.storage, () => "thread");
      goals.create({ objective: "Ship" });
      goals.updateByUser({ status: "paused" });
      for (const [id, text] of [["status", "/goal"], ["edit", "/goal edit Ship all"], ["pause", "/goal pause"], ["clear", "/goal clear"]]) {
        const response = await session.fetch(new Request("https://session.internal/turns", { method: "POST", body: JSON.stringify({ id, input: [{ type: "text", text }] }) }));
        expect(response.status).toBe(202);
        expect(await response.json()).toMatchObject({ turn_id: id, state: "completed" });
      }
      expect(goals.get()).toBeNull();
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_turns WHERE may_have_inner_operation=1").one().n).toBe(0);
      await state.storage.deleteAlarm();
    });
  });
  it("recovers a retained command receipt without executing it again or loading the model", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      stubMemory(session);
      state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,'thread','owner','org','team',1,'https://example.com','managed',?)`, Date.now());
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      goals.create({ objective: "Preserve this goal" });
      const result = runtime.command("pause");
      runtime.retainCommand("interrupted-command", result, goals.get()!.goalId);
      state.storage.sql.exec(`INSERT INTO managed_turns(id,request_hash,input_json,authorization_json,state,accepted_cursor,created_at,accepted_at,updated_at,may_have_inner_operation) VALUES('interrupted-command','hash',?, '{"capabilities":[]}', 'accepted',1,?,?,?,0)`, JSON.stringify("/goal pause"), Date.now(), Date.now(), Date.now());
      await session.alarm();
      for (let n = 0; n < 100 && state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_turns WHERE id='interrupted-command'").one().state !== "completed"; n++) await new Promise(resolve => setTimeout(resolve, 10));
      expect(state.storage.sql.exec<{ state: string; may_have_inner_operation: number }>("SELECT state,may_have_inner_operation FROM managed_turns WHERE id='interrupted-command'").one()).toEqual({ state: "completed", may_have_inner_operation: 0 });
      expect(goals.get()).toMatchObject({ objective: "Preserve this goal", status: "paused" });
      await state.storage.deleteAlarm();
    });
  });
  it("drops a persisted continuation when the authorization epoch changes", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (session, state) => {
      stubMemory(session);
      state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,'thread','owner','org','team',2,'https://example.com','managed',?)`, Date.now());
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      goals.create({ objective: "Never widen authority" });
      runtime.bind("source", 1);
      runtime.finish("source", true, true);
      await session.alarm();
      for (let n = 0; n < 100 && runtime.pending(); n++) await new Promise(resolve => setTimeout(resolve, 10));
      expect(runtime.pending()).toBeUndefined();
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_turns").one().n).toBe(0);
      runtime.clear();
      await state.storage.deleteAlarm();
    });
  });

  it("preserves a newer resume when the paused turn finishes cancelling", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_session, state) => {
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      runtime.command("Ship"); runtime.bind("old", 1);
      runtime.command("pause"); runtime.command("resume"); runtime.bind("resume", 1);
      runtime.finish("resume", true, true);
      runtime.finish("old", false, true, "paused");
      expect(goals.get()?.status).toBe("active");
      expect(runtime.pending()?.turn_id).toBe("resume");
      runtime.command("edit Ship everything");
      expect(() => runtime.assertCurrentObjective("resume")).toThrow("changed");
      runtime.acknowledgeObjective("resume", goals.get());
      expect(() => runtime.assertCurrentObjective("resume")).not.toThrow();
    });
  });
  it("stops budgeted work on missing provider usage and freezes elapsed time at stop", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_session, state) => {
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      goals.create({ objective: "Ship", token_budget: 100 }); runtime.bind("turn", 1);
      state.storage.sql.exec("INSERT INTO managed_model_usage VALUES('m',1,'root','turn','model.call.completed',0,'{}')");
      expect(runtime.flush("turn")?.status).toBe("usageLimited");
      runtime.flush("turn");
      const seconds = goals.get()!.timeUsedSeconds;
      runtime.flush("turn", Date.now() + 100_000);
      expect(goals.get()!.timeUsedSeconds).toBe(seconds);
      runtime.finish("turn", true, true);
      expect(runtime.pending()).toBeUndefined();
    });
  });

});
