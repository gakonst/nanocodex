import type { ToolContext } from "nanocodex";
import { taskPlan, taskSchemas } from "./claude-engine-runtime.mjs";

/** Pure Rust transitions are committed with their receipts under one DO transaction. */
export function claudeBoard(storage: DurableObjectStorage, authorize: (context: ToolContext) => void) {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_claude_board (session_id TEXT PRIMARY KEY, checkpoint TEXT NOT NULL)`);
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_claude_board_receipts (session_id TEXT NOT NULL, call_id TEXT NOT NULL, request TEXT NOT NULL, output TEXT NOT NULL, PRIMARY KEY(session_id,call_id))`);
  let pending = Promise.resolve();
  return taskSchemas().map(definition => ({ name: definition.name, description: definition.description, inputSchema: definition.input_schema,
    handler: async (input: unknown, context: ToolContext) => {
      const prior = pending;
      let release!: () => void;
      pending = new Promise<void>(resolve => { release = resolve; });
      await prior;
      try {
        const check = () => { authorize(context); context.signal.throwIfAborted(); };
        check();
        if (!context.sessionId || !context.callId) throw new Error("task board requires a stable session and call identity");
        const request = JSON.stringify({ name: definition.name, input });
        const receipt = storage.sql.exec<{request: string; output: string}>("SELECT request,output FROM managed_claude_board_receipts WHERE session_id=? AND call_id=?", context.sessionId, context.callId).toArray()[0];
        if (receipt) {
          if (receipt.request !== request) throw new Error("task call identity already bound to another input");
          return {content: receipt.output, isError: false};
        }
        const saved = storage.sql.exec<{checkpoint: string}>("SELECT checkpoint FROM managed_claude_board WHERE session_id=?", context.sessionId).toArray()[0];
        const plan = await taskPlan({name: definition.name, input, checkpoint: saved ? JSON.parse(saved.checkpoint) : null});
        check();
        storage.transactionSync(() => {
          storage.sql.exec("INSERT OR REPLACE INTO managed_claude_board VALUES (?,?)", context.sessionId, JSON.stringify(plan.checkpoint));
          storage.sql.exec("INSERT INTO managed_claude_board_receipts VALUES (?,?,?,?)", context.sessionId, context.callId, request, plan.output);
        });
        return {content: plan.output, isError: false};
      } finally { release(); }
    }
  }));
}
