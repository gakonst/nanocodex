import { GOAL_CONTINUATION_TEMPLATE } from "./goal-continuation";
import { renderCodexTemplate } from "./codex-prompts";
/** Persisted, session-local goal state. User controls and model tools are deliberately separate. */
export type GoalStatus = "active" | "paused" | "blocked" | "usageLimited" | "budgetLimited" | "complete";
export interface ThreadGoal {
  goalId: string;
  threadId: string;
  objective: string;
  status: GoalStatus;
  tokenBudget?: number;
  tokensUsed: number;
  timeUsedSeconds: number;
  createdAt: number;
  updatedAt: number;
}
export type GoalResponse = { goal: ThreadGoal | null; remainingTokens: number | null; completionBudgetReport: string | null };
export type GoalUserUpdate = { objective?: string; status?: GoalStatus; tokenBudget?: number | null };

function objective(value: unknown): string {
  if (typeof value !== "string" || !value.trim() || [...value.trim()].length > 4_000) {
    throw new TypeError("goal objective must contain 1–4000 characters");
  }
  return value.trim();
}
function budget(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) throw new TypeError("token budget must be a positive safe integer");
  return value;
}
export function goalResponse(goal: ThreadGoal | null, completion = false): GoalResponse {
  return { goal, remainingTokens: goal?.tokenBudget === undefined ? null : Math.max(0, goal.tokenBudget - goal.tokensUsed),
    completionBudgetReport: completion && goal?.tokenBudget !== undefined
      ? `Goal completed using ${goal.tokensUsed} of ${goal.tokenBudget} budgeted tokens.` : null };
}

export class Goals {
  constructor(private readonly storage: DurableObjectStorage, private readonly threadId: () => string, private readonly now = Date.now) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_goal (singleton INTEGER PRIMARY KEY CHECK(singleton=1), body TEXT NOT NULL)`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_goal_usage (goal_id TEXT NOT NULL, turn_id TEXT NOT NULL, tokens INTEGER NOT NULL, seconds REAL NOT NULL, PRIMARY KEY(goal_id,turn_id))`);
  }
  get(): ThreadGoal | null {
    const row = this.storage.sql.exec<{ body: string }>("SELECT body FROM managed_goal WHERE singleton=1").toArray()[0];
    return row ? JSON.parse(row.body) as ThreadGoal : null;
  }
  private save(goal: ThreadGoal): ThreadGoal {
    this.storage.sql.exec("INSERT INTO managed_goal(singleton,body) VALUES(1,?) ON CONFLICT(singleton) DO UPDATE SET body=excluded.body", JSON.stringify(goal));
    return goal;
  }
  create(input: { objective: string; token_budget?: number }): ThreadGoal {
    const text = objective(input.objective);
    const tokenBudget = input.token_budget === undefined ? undefined : budget(input.token_budget);
    return this.storage.transactionSync(() => {
      const current = this.get();
      if (current && current.status !== "complete") throw new Error("cannot create a new goal because this thread has an unfinished goal");
      this.storage.sql.exec("DELETE FROM managed_goal_usage");
      const now = this.now();
      return this.save({ goalId: crypto.randomUUID(), threadId: this.threadId(), objective: text, status: "active", tokenBudget,
        tokensUsed: 0, timeUsedSeconds: 0, createdAt: now, updatedAt: now });
    });
  }
  /** Only expose to an authenticated user control, never as model tool arguments. */
  updateByUser(update: GoalUserUpdate): ThreadGoal {
    const text = update.objective === undefined ? undefined : objective(update.objective);
    const tokenBudget = update.tokenBudget == null ? update.tokenBudget : budget(update.tokenBudget);
    if (update.status !== undefined && !["active", "paused", "blocked", "usageLimited", "budgetLimited", "complete"].includes(update.status)) throw new TypeError("invalid goal status");
    return this.storage.transactionSync(() => {
      const goal = this.get();
      if (!goal) throw new Error("this thread has no goal");
      if (text !== undefined) goal.objective = text;
      if (tokenBudget !== undefined) goal.tokenBudget = tokenBudget ?? undefined;
      if (update.status !== undefined) goal.status = update.status;
      if (goal.tokenBudget !== undefined && goal.tokensUsed >= goal.tokenBudget && goal.status !== "complete") goal.status = "budgetLimited";
      goal.updatedAt = this.now();
      return this.save(goal);
    });
  }
  updateByModel(status: "complete" | "blocked" | "paused"): ThreadGoal {
    if (!["complete", "blocked", "paused"].includes(status)) throw new TypeError("update_goal can only mark a goal complete, blocked, or paused");
    const goal = this.get();
    if (!goal) throw new Error("this thread has no goal");
    // A model cannot undo a system-enforced stop or resume a stopped goal.
    if (goal.status === "budgetLimited" || goal.status === "usageLimited") return goal;
    return this.updateByUser({ status });
  }
  /** Cumulative snapshots for one turn; replay and increasing partial snapshots are safe.
   * Capture goalId at turn admission, so an old turn cannot charge a replacement goal.
   * Tokens should be uncached input + output tokens (the upstream goal accounting policy).
   */
  accountTurn(goalId: string, turnId: string, tokens: number, seconds: number): ThreadGoal | null {
    if (!Number.isSafeInteger(tokens) || tokens < 0 || !Number.isFinite(seconds) || seconds < 0) throw new TypeError("invalid goal usage");
    return this.storage.transactionSync(() => {
      const goal = this.get();
      if (!goal || goal.goalId !== goalId) return goal;
      const previous = this.storage.sql.exec<{ tokens: number; seconds: number }>("SELECT tokens,seconds FROM managed_goal_usage WHERE goal_id=? AND turn_id=?", goalId, turnId).toArray()[0];
      const nextTokens = Math.max(previous?.tokens ?? 0, tokens);
      const nextSeconds = Math.max(previous?.seconds ?? 0, seconds);
      goal.tokensUsed += nextTokens - (previous?.tokens ?? 0);
      goal.timeUsedSeconds += nextSeconds - (previous?.seconds ?? 0);
      if (!Number.isSafeInteger(goal.tokensUsed)) throw new Error("goal token usage overflow");
      this.storage.sql.exec("INSERT INTO managed_goal_usage VALUES(?,?,?,?) ON CONFLICT(goal_id,turn_id) DO UPDATE SET tokens=excluded.tokens,seconds=excluded.seconds", goalId, turnId, nextTokens, nextSeconds);
      if (goal.tokenBudget !== undefined && goal.tokensUsed >= goal.tokenBudget && goal.status !== "complete") goal.status = "budgetLimited";
      goal.updatedAt = this.now();
      return this.save(goal);
    });
  }
  clear(): void {
    this.storage.transactionSync(() => {
      this.storage.sql.exec("DELETE FROM managed_goal");
      this.storage.sql.exec("DELETE FROM managed_goal_usage");
    });
  }
}

/** Call only after successful turn completion; errors/cancellation must not auto-resume. */
export function goalContinuation(goal: ThreadGoal | null): string | null {
  if (!goal || goal.status !== "active") return null;
  const escaped = goal.objective.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
  const values: Record<string, string> = { objective: escaped, tokens_used: String(goal.tokensUsed),
    token_budget: goal.tokenBudget === undefined ? "none" : String(goal.tokenBudget),
    remaining_tokens: goal.tokenBudget === undefined ? "unbounded" : String(Math.max(0, goal.tokenBudget - goal.tokensUsed)) };
  return renderCodexTemplate(GOAL_CONTINUATION_TEMPLATE, values);
}
