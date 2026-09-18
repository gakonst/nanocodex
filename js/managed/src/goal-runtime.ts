import { Goals, goalResponse, type ThreadGoal } from "./goals";

export function parseGoalCommand(input: unknown): string | null {
  if (Array.isArray(input)) {
    const text = input.filter(part => part && part.type === "text" && typeof part.text === "string").map(part => part.text).join("\n");
    if (/^\/goal(?:\s|$)/.test(text.trim()) && input.some(part => !part || part.type !== "text")) throw new Error("/goal commands cannot include attachments");
    input = text;
  }
  if (typeof input !== "string") return null;
  const match = /^\/goal(?:\s+([\s\S]*))?$/.exec(input.trim());
  return match ? (match[1] ?? "").trim() : null;
}
export type GoalTurn = { turn_id: string; goal_id: string; epoch: number; started_at: number; pending: number; empty_turns: number; tokens_baseline: number; objective: string; stopped_at: number | null };
/** Durable continuation outbox and accounting bindings owned by the session. */
export class GoalRuntime {
  constructor(readonly storage: DurableObjectStorage, readonly goals: Goals) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_goal_commands (turn_id TEXT PRIMARY KEY, result TEXT NOT NULL)`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_goal_turns (turn_id TEXT PRIMARY KEY, goal_id TEXT NOT NULL, epoch INTEGER NOT NULL, started_at INTEGER NOT NULL, pending INTEGER NOT NULL DEFAULT 0, empty_turns INTEGER NOT NULL DEFAULT 0, tokens_baseline INTEGER NOT NULL DEFAULT 0, objective TEXT NOT NULL, stopped_at INTEGER)`);
  }
  command(command: string): { text: string; continue: boolean; goal: ThreadGoal | null } {
    let goal: ThreadGoal | null;
    let resume = false;
    if (command === "help") return { text: "/goal <objective> · /goal status · /goal edit <objective> · /goal pause · /goal resume · /goal budget <tokens|none> · /goal clear", continue: false, goal: this.goals.get() };
    if (["edit", "set"].includes(command)) throw new Error(`Usage: /goal ${command} <objective>`);
    if (!command || command === "status") goal = this.goals.get();
    else if (command === "clear") { this.stopAll(); this.goals.clear(); this.discardPending(); goal = null; }
    else if (command === "pause") { this.stopAll(); goal = this.goals.updateByUser({ status: "paused" }); this.discardPending(); }
    else if (command === "resume") { goal = this.goals.updateByUser({ status: "active" }); resume = goal.status === "active"; }
    else if (/^budget(?:\s|$)/.test(command)) {
      const value = command.slice(6).trim();
      if (value !== "none" && !/^[1-9]\d*$/.test(value)) throw new Error("Usage: /goal budget <positive tokens|none>");
      goal = this.goals.updateByUser({ tokenBudget: value === "none" ? null : Number(value) });
    } else if (command.startsWith("edit ")) {
      goal = this.goals.updateByUser({ objective: command.slice(5) });
    } else {
      const text = command.startsWith("set ") ? command.slice(4) : command;
      const current = this.goals.get();
      if (current && current.status !== "complete") throw new Error("An unfinished goal exists. Use /goal edit <objective> or /goal clear first.");
      goal = this.goals.create({ objective: text });
      resume = goal.status === "active";
    }
    return { text: goal ? `Goal ${goal.status}: ${goal.objective}\nTokens used: ${goal.tokensUsed}${goal.tokenBudget === undefined ? " (unlimited budget)" : ` / ${goal.tokenBudget}; remaining: ${goalResponse(goal).remainingTokens}`}\nActive time: ${Math.floor(goal.timeUsedSeconds)} seconds.` : "No goal is set.", continue: resume, goal };
  }
  retainCommand(id: string, result: { text: string; continue: boolean }, controlledGoalId?: string, epoch = 0): void {
    this.storage.sql.exec("INSERT INTO managed_goal_commands VALUES(?,?)", id, JSON.stringify({ ...result, controlledGoalId, epoch, goalId: this.goals.get()?.goalId }));
  }
  retainedCommand(id: string): { text: string; continue: boolean; controlledGoalId?: string; goalId?: string; epoch: number } | undefined {
    const row = this.storage.sql.exec<{ result: string }>("SELECT result FROM managed_goal_commands WHERE turn_id=?", id).toArray()[0];
    return row ? JSON.parse(row.result) : undefined;
  }
  bind(turnId: string, epoch: number, queued = false): void {
    const goal = this.goals.get();
    if (goal?.status !== "active") return;
    const existing = this.turn(turnId);
    if (existing?.goal_id === goal.goalId) {
      if (!queued && existing.started_at === 0) this.storage.sql.exec("UPDATE managed_goal_turns SET started_at=?,tokens_baseline=? WHERE turn_id=? AND started_at=0", Date.now(), this.tokens(turnId), turnId);
      return;
    }
    this.storage.sql.exec("INSERT INTO managed_goal_turns(turn_id,goal_id,epoch,started_at,tokens_baseline,objective) VALUES(?,?,?,?,?,?) ON CONFLICT(turn_id) DO UPDATE SET goal_id=excluded.goal_id,epoch=excluded.epoch,started_at=excluded.started_at,tokens_baseline=excluded.tokens_baseline,pending=0,empty_turns=0,objective=excluded.objective,stopped_at=NULL", turnId, goal.goalId, epoch, queued ? 0 : Date.now(), this.tokens(turnId), goal.objective);
  }
  stop(id: string): void {
    this.storage.sql.exec("UPDATE managed_goal_turns SET stopped_at=COALESCE(stopped_at,?) WHERE turn_id=?", Date.now(), id);
  }
  private stopAll(): void {
    const goal = this.goals.get();
    if (goal) this.storage.sql.exec("UPDATE managed_goal_turns SET stopped_at=COALESCE(stopped_at,?) WHERE goal_id=?", Date.now(), goal.goalId);
  }
  assertCurrentObjective(id: string): void {
    const binding = this.turn(id), goal = this.goals.get();
    if (!binding || !goal || binding.goal_id !== goal.goalId || binding.objective !== goal.objective) throw new Error("The goal changed during this turn. Call get_goal and audit the current objective before updating its status.");
  }
  acknowledgeObjective(id: string, goal: ThreadGoal | null): void {
    if (goal) this.storage.sql.exec("UPDATE managed_goal_turns SET objective=? WHERE turn_id=? AND goal_id=?", goal.objective, id, goal.goalId);
  }
  turn(id: string): GoalTurn | undefined {
    return this.storage.sql.exec<GoalTurn>("SELECT * FROM managed_goal_turns WHERE turn_id=?", id).toArray()[0];
  }
  flush(id: string, now = Date.now()): ThreadGoal | null {
    const binding = this.turn(id);
    if (!binding || binding.started_at === 0) return this.goals.get();
    const goal = this.goals.get();
    if (!goal || goal.goalId !== binding.goal_id) return goal;
    if (goal.status !== "active" && binding.stopped_at === null) {
      binding.stopped_at = Math.min(now, goal.updatedAt);
      this.storage.sql.exec("UPDATE managed_goal_turns SET stopped_at=? WHERE turn_id=?", binding.stopped_at, id);
    }
    const unknown = this.storage.sql.exec<{ payload: string }>("SELECT payload FROM managed_model_usage WHERE turn_id=?", id).toArray().some(row => {
      const usage = (JSON.parse(row.payload) as { usage?: { input_tokens?: unknown; output_tokens?: unknown } }).usage;
      return !usage || typeof usage.input_tokens !== "number" || typeof usage.output_tokens !== "number";
    });
    const accounted = this.goals.accountTurn(binding.goal_id, id, Math.max(0, this.tokens(id) - binding.tokens_baseline), Math.max(0, (binding.stopped_at ?? now) - binding.started_at) / 1000);
    if (unknown && accounted?.tokenBudget !== undefined && accounted.status === "active") return this.goals.updateByUser({ status: "usageLimited" });
    return accounted;
  }
  private tokens(id: string): number {
    let tokens = 0;
    for (const row of this.storage.sql.exec<{ payload: string }>("SELECT payload FROM managed_model_usage WHERE turn_id=?", id)) {
      const usage = (JSON.parse(row.payload) as { usage?: { input_tokens?: number; output_tokens?: number; cached_input_tokens?: number; input_tokens_details?: { cached_tokens?: number } } }).usage;
      if (usage) tokens += Math.max(0, (usage.input_tokens ?? 0) - (usage.input_tokens_details?.cached_tokens ?? usage.cached_input_tokens ?? 0)) + (usage.output_tokens ?? 0);
    }
    return tokens;
  }

  finish(id: string, successful: boolean, meaningful: boolean, stopStatus: "paused" | "blocked" | "usageLimited" = "blocked"): void {
    const binding = this.turn(id);
    if (!binding) return;
    this.stop(id);
    const goal = this.flush(id);
    const superseded = this.storage.sql.exec("SELECT turn_id FROM managed_goal_turns WHERE goal_id=? AND rowid > (SELECT rowid FROM managed_goal_turns WHERE turn_id=?) LIMIT 1", binding.goal_id, id).toArray().length > 0;
    const previous = this.storage.sql.exec<{ empty_turns: number }>("SELECT empty_turns FROM managed_goal_turns WHERE goal_id=? AND turn_id<>? ORDER BY started_at DESC,rowid DESC LIMIT 1", binding.goal_id, id).toArray()[0]?.empty_turns ?? 0;
    const empty = meaningful ? 0 : previous + 1;
    if (!superseded && goal?.goalId === binding.goal_id && goal.status === "active") {
      if (successful && empty >= 3) this.goals.updateByUser({ status: "blocked" });
      else if (!successful) this.goals.updateByUser({ status: stopStatus });
    }
    this.storage.sql.exec("UPDATE managed_goal_turns SET pending=0 WHERE goal_id=? AND rowid <= (SELECT rowid FROM managed_goal_turns WHERE turn_id=?)", binding.goal_id, id);
    const newerPending = this.pending();
    this.storage.sql.exec("UPDATE managed_goal_turns SET pending=?,empty_turns=? WHERE turn_id=?", Number(!superseded && !newerPending && successful && goal?.goalId === binding.goal_id && this.goals.get()?.status === "active" && empty < 3), empty, id);
    if (!successful) this.storage.sql.exec("UPDATE managed_goal_turns SET pending=0 WHERE turn_id=?", id);
  }
  pending(): GoalTurn | undefined {
    return this.storage.sql.exec<GoalTurn>("SELECT * FROM managed_goal_turns WHERE pending=1 ORDER BY started_at DESC,rowid DESC LIMIT 1").toArray()[0];
  }
  discardPending(): void { this.storage.sql.exec("UPDATE managed_goal_turns SET pending=0 WHERE pending=1"); }
  clear(): void { this.goals.clear(); this.storage.sql.exec("DELETE FROM managed_goal_turns"); this.storage.sql.exec("DELETE FROM managed_goal_commands"); }
}
