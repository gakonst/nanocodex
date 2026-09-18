import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { ToolContext } from "nanocodex";
import type { DurableAgentSession } from "../src/index";
import { Goals, goalContinuation, goalResponse } from "../src/goals";
import { createGoalTools } from "../src/goal-tools";
import { GoalRuntime } from "../src/goal-runtime";

async function withGoals(test: (goals: Goals, state: DurableObjectState) => void | Promise<void>) {
  const ns = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(ns.get(ns.newUniqueId()), async (_instance, state) => test(new Goals(state.storage, () => "thread", () => 1000), state));
}
const context = { callId: "goal-call", parentCallId: "cell", sessionId: "thread", model: "test", signal: new AbortController().signal } satisfies ToolContext;

describe("persisted managed goals", () => {
  it("persists across store instances and only replaces complete goals", async () => withGoals((goals, state) => {
    expect(goals.get()).toBeNull();
    const goal = goals.create({ objective: "  Ship the change  " });
    expect(new Goals(state.storage, () => "thread").get()).toEqual(goal);
    expect(goal.objective).toBe("Ship the change");
    expect(() => goals.create({ objective: "other" })).toThrow("unfinished");
    goals.updateByModel("paused");
    expect(goalContinuation(goals.get())).toBeNull();
    expect(() => goals.create({ objective: "other" })).toThrow("unfinished");
    goals.updateByUser({ status: "active" });
    expect(goalContinuation(goals.get())).toContain("Ship the change");
    goals.updateByModel("complete");
    const next = goals.create({ objective: "next" });
    expect(next.goalId).not.toBe(goal.goalId);
    expect(goals.accountTurn(goal.goalId, "old", 50, 10)).toEqual(next);
  }));

  it("accounts cumulative snapshots once, enforces budgets, and resumes via user controls", async () => withGoals(goals => {
    const goal = goals.create({ objective: "Ship", token_budget: 100 });
    goals.accountTurn(goal.goalId, "turn", 40, 3);
    goals.accountTurn(goal.goalId, "turn", 40, 3);
    goals.accountTurn(goal.goalId, "turn", 30, 2);
    expect(goals.get()).toMatchObject({ tokensUsed: 40, timeUsedSeconds: 3 });
    goals.accountTurn(goal.goalId, "turn", 70, 5);
    goals.accountTurn(goal.goalId, "next", 35, 2);
    expect(goalResponse(goals.get())).toMatchObject({ remainingTokens: 0, goal: { status: "budgetLimited", tokensUsed: 105, timeUsedSeconds: 7 } });
    expect(goals.updateByModel("paused").status).toBe("budgetLimited");
    expect(goals.updateByUser({ status: "active" }).status).toBe("budgetLimited");
    expect(goals.updateByUser({ status: "active", tokenBudget: 200 }).status).toBe("active");
    expect(goals.updateByUser({ tokenBudget: null }).tokenBudget).toBeUndefined();
    goals.clear();
    expect(goals.get()).toBeNull();
  }));

  it("validates Unicode objectives, budgets and usage", async () => withGoals(goals => {
    expect(() => goals.create({ objective: " " })).toThrow();
    expect(() => goals.create({ objective: "x".repeat(4001) })).toThrow();
    for (const token_budget of [0, -1, 1.5, Infinity, Number.MAX_SAFE_INTEGER + 1]) expect(() => goals.create({ objective: "x", token_budget })).toThrow();
    const goal = goals.create({ objective: "😀".repeat(4000) });
    expect(() => goals.accountTurn(goal.goalId, "turn", -1, 2)).toThrow();
    expect(() => goals.accountTurn(goal.goalId, "turn", 1, NaN)).toThrow();
  }));

  it("keeps model controls narrow and flushes accounting before completion", async () => withGoals(async goals => {
    let flushes = 0;
    const tools = createGoalTools(goals, () => { flushes++; const goal = goals.get(); if (goal) goals.accountTurn(goal.goalId, "turn", 12, 2); });
    const [get, create, update] = tools;
    expect(tools.map(t => t.name)).toEqual(["get_goal", "create_goal", "update_goal"]);
    for (const input of [null, [], { objective: "x", authorization: "override" }, { objective: "x", token_budget: null }]) await expect(async () => create!.handler(input, context)).rejects.toThrow();
    await create!.handler({ objective: "Ship", token_budget: 100 }, context);
    await expect(async () => update!.handler({ status: "active" }, context)).rejects.toThrow();
    await expect(async () => update!.handler({ status: "complete", objective: "narrower" }, context)).rejects.toThrow();
    expect(await update!.handler({ status: "complete" }, context)).toMatchObject({ completionBudgetReport: "Goal completed using 12 of 100 budgeted tokens.", goal: { status: "complete" } });
    await get!.handler({}, context);
    expect(flushes).toBe(2);
  }));
  it("revalidates after async accounting and acknowledges only the returned snapshot", async () => withGoals(async (goals, state) => {
    const runtime = new GoalRuntime(state.storage, goals);
    goals.create({ objective: "Original" }); runtime.bind("turn", 1);
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    const tools = createGoalTools(goals, () => gate, {
      beforeUpdate: () => runtime.assertCurrentObjective("turn"),
      onRead: snapshot => runtime.acknowledgeObjective("turn", snapshot),
    });
    const completing = tools[2]!.handler({ status: "complete" }, context);
    goals.updateByUser({ objective: "Changed during accounting" });
    release();
    await expect(completing).rejects.toThrow("changed");
    const response = await tools[0]!.handler({}, context) as { goal: NonNullable<ReturnType<Goals["get"]>> };
    goals.updateByUser({ objective: "Changed after read" });
    runtime.acknowledgeObjective("turn", response.goal);
    expect(() => runtime.assertCurrentObjective("turn")).toThrow("changed");
    expect(goals.get()?.status).toBe("active");
  }));

});
