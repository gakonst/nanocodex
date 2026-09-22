import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { Goals } from "../src/goals";
import { GoalRuntime } from "../src/goal-runtime";

const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;

describe("goal continuation races", () => {
  it("preserves a resumed goal's continuation when the paused turn finishes cancellation late", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_session, state) => {
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      const original = goals.create({ objective: "Finish the requested work" });
      runtime.bind("old-running-turn", 7);
      runtime.command("pause");
      runtime.command("resume");
      runtime.bind("resume-command", 7);
      runtime.finish("resume-command", true, true);
      expect(runtime.pending()?.turn_id).toBe("resume-command");

      // The cancellation acknowledgement belongs to the run before resume.
      runtime.finish("old-running-turn", false, false, "paused");
      const recovered = new GoalRuntime(state.storage, goals);
      expect(goals.get()).toMatchObject({ goalId: original.goalId, status: "active" });
      expect(recovered.pending()).toMatchObject({ turn_id: "resume-command", goal_id: original.goalId });
    });
  });

  it("preserves a replacement goal's continuation when a cleared goal's turn is cancelled late", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_session, state) => {
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      goals.create({ objective: "Old work" });
      runtime.bind("old-running-turn", 7);
      runtime.command("clear");
      const replacement = runtime.command("New requested work").goal!;
      runtime.bind("new-goal-command", 7);
      runtime.finish("new-goal-command", true, true);

      runtime.finish("old-running-turn", false, false, "paused");
      const recovered = new GoalRuntime(state.storage, goals);
      expect(goals.get()).toMatchObject({ goalId: replacement.goalId, status: "active", tokensUsed: 0 });
      expect(recovered.pending()).toMatchObject({ turn_id: "new-goal-command", goal_id: replacement.goalId });
    });
  });

  it.each([
    ["absent usage", {}],
    ["missing output count", { usage: { input_tokens: 10 } }],
    ["missing input count", { usage: { output_tokens: 10 } }],
  ])("stops budgeted continuation with %s even when the model produces a final answer", async (_label, payload) => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_session, state) => {
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      goals.create({ objective: "Finish the requested work", token_budget: 100 });
      runtime.bind("model-turn", 7);
      state.storage.sql.exec(
        "INSERT INTO managed_model_usage VALUES(?,1,'root',?,'model.call.completed',0,?)",
        crypto.randomUUID(), "model-turn", JSON.stringify(payload),
      );
      runtime.finish("model-turn", true, true);

      const recovered = new GoalRuntime(state.storage, goals);
      expect(goals.get()?.status).toBe("usageLimited");
      expect(recovered.pending()).toBeUndefined();
    });
  });

  it("allows budgeted continuation when measured usage is explicitly zero", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_session, state) => {
      const goals = new Goals(state.storage, () => "thread");
      const runtime = new GoalRuntime(state.storage, goals);
      goals.create({ objective: "Finish the requested work", token_budget: 100 });
      runtime.bind("model-turn", 7);
      state.storage.sql.exec(
        "INSERT INTO managed_model_usage VALUES(?,1,'root',?,'model.call.completed',0,?)",
        crypto.randomUUID(), "model-turn", JSON.stringify({ usage: { input_tokens: 0, output_tokens: 0 } }),
      );
      runtime.finish("model-turn", true, true);
      expect(goals.get()).toMatchObject({ status: "active", tokensUsed: 0 });
      expect(runtime.pending()?.turn_id).toBe("model-turn");
    });
  });
});
