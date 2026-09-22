import type { NamedTool, ToolContext } from "nanocodex";
import { Goals, goalResponse, type ThreadGoal } from "./goals";

function args(input: unknown, allowed: string[]): Record<string, unknown> {
  if (!input || typeof input !== "object" || Array.isArray(input) || Object.keys(input).some(key => !allowed.includes(key))) throw new TypeError("invalid goal tool arguments");
  return input as Record<string, unknown>;
}
/** beforeRead flushes current model usage before returning tool/accounting results. */
export function createGoalTools(goals: Goals, beforeRead?: (context: ToolContext) => void | Promise<void>, hooks?: {
  beforeUpdate?: (context: ToolContext) => void;
  onRead?: (goal: ThreadGoal | null, context: ToolContext) => void;
}): NamedTool[] {
  return [
    { name: "get_goal", description: "Get the persisted goal for this thread, its status, token and elapsed-time usage, and remaining token budget.",
      parameters: { type: "object", properties: {}, required: [], additionalProperties: false },
      handler: async (input, context) => {
        args(input, []); await beforeRead?.(context);
        const goal = goals.get();
        hooks?.onRead?.(goal, context);
        return goalResponse(goal);
      } },
    { name: "create_goal", description: "Create a goal only when explicitly requested by the user or system/developer instructions; do not infer goals from ordinary tasks. Set token_budget only when explicitly requested. Fails if an unfinished goal exists.",
      parameters: { type: "object", properties: { objective: { type: "string", minLength: 1, maxLength: 4000 }, token_budget: { type: "integer", minimum: 1 } }, required: ["objective"], additionalProperties: false },
      handler: async (input) => {
        const value = args(input, ["objective", "token_budget"]);
        return goalResponse(goals.create(value as { objective: string; token_budget?: number }));
      } },
    { name: "update_goal", description: "Update the existing goal status. Mark complete only when the full objective is achieved and no required work remains; for budgeted goals report final token usage from the result. Mark blocked only when the same genuine blocking condition has recurred for at least three consecutive goal turns and meaningful progress requires user input or external change. Resuming a blocked goal starts a fresh audit. Mark paused only at the user's explicit request, report the returned status, and stop goal work. Budget limits take precedence. Never mark complete merely because the budget is nearly exhausted. Resume and budget changes are user/system controls.",
      parameters: { type: "object", properties: { status: { type: "string", enum: ["complete", "blocked", "paused"] } }, required: ["status"], additionalProperties: false },
      handler: async (input, context) => {
        const value = args(input, ["status"]);
        if (value.status !== "complete" && value.status !== "blocked" && value.status !== "paused") throw new TypeError("invalid goal status");
        await beforeRead?.(context);
        hooks?.beforeUpdate?.(context);
        const goal = goals.updateByModel(value.status);
        return goalResponse(goal, goal.status === "complete");
      } },
  ];
}
