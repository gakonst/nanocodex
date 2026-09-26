import type { NamedTool, ToolContext } from "nanocodex";
import { tracing } from "cloudflare:workers";

type Sql = DurableObjectStorage["sql"];
type ToolRow = {
  internal_turn_id: string; call_id: string; tool: string | null;
  started_at: number | null; started_ms: number | null;
  result_ms: number | null; duration_ms: number | null; status: string | null;
};
type PhaseRow = { phase: string; duration_ms: number; count: number };

/** Content-free tool timeline. Each phase belongs to the internal Agent turn,
 * so provider call IDs can be reused on separate turns without collisions. */
export class ToolTiming {
  private readonly pending = new Map<string, PhaseRow[]>();
  private schema: "unknown" | "absent" | "ready" = "unknown";

  constructor(private readonly sql: Sql) {}

  private ensureSchema(): number | null {
    if (this.schema === "ready") return null;
    const start = performance.now();
    this.sql.exec(`CREATE TABLE IF NOT EXISTS managed2_tool_timing (
      internal_turn_id TEXT NOT NULL, call_id TEXT NOT NULL,
      external_turn_id TEXT, tool TEXT, started_at INTEGER, started_ms INTEGER,
      result_ms INTEGER, duration_ms REAL, status TEXT,
      PRIMARY KEY (internal_turn_id, call_id)
    )`);
    this.sql.exec(`CREATE INDEX IF NOT EXISTS managed2_tool_timing_turn
      ON managed2_tool_timing (external_turn_id, started_ms)`);
    this.sql.exec(`CREATE TABLE IF NOT EXISTS managed2_tool_phase (
      internal_turn_id TEXT NOT NULL, call_id TEXT NOT NULL,
      phase TEXT NOT NULL, duration_ms REAL NOT NULL, count INTEGER NOT NULL,
      PRIMARY KEY (internal_turn_id, call_id, phase)
    )`);
    this.schema = "ready";
    return performance.now() - start;
  }

  instrument(tool: NamedTool, correlation: (context: ToolContext) => string | undefined): NamedTool {
    return {
      ...tool,
      handler: (input, context) => tracing.enterSpan("managed2.tool", async span => {
        // Tool names are from this fixed local registration, never model input.
        span.setAttribute("managed2.tool.name", tool.name);
        const traceId = correlation(context);
        if (traceId) span.setAttribute("managed2.trace_id", traceId);
        const began = performance.now();
        try {
          const result = await tool.handler(input, context);
          span.setAttribute("managed2.outcome", "completed");
          return result;
        } catch (error) {
          span.setAttribute("managed2.outcome", "failed");
          throw error;
        } finally { this.phase(context, "handler", performance.now() - began); }
      }),
    };
  }

  /** Use a unique in-flight provider call ID, never a DO-wide active-turn guess. */
  correlation(context: ToolContext): string | undefined {
    if (this.schema === "absent") return undefined;
    const rows = this.sql.exec<{ trace_id: string }>(`SELECT t.trace_id FROM managed2_tool_timing AS c
      JOIN turn_timing AS t ON t.id = c.external_turn_id
      WHERE c.call_id = ? AND c.status IS NULL`, context.callId).toArray();
    return rows.length === 1 ? rows[0]!.trace_id : undefined;
  }

  externalTurn(context: ToolContext): string | undefined {
    this.ensureSchema();
    const rows = this.sql.exec<{ external_turn_id: string }>(
      "SELECT external_turn_id FROM managed2_tool_timing WHERE call_id = ? AND status IS NULL",
      context.callId).toArray();
    return rows.length === 1 ? rows[0]!.external_turn_id : undefined;
  }

  phase(context: ToolContext, phase: string, durationMs: number): void {
    // ToolContext.turnId identifies a JS execution, not the Rust Agent event
    // turn_id. Pair through the call ID emitted by tool.call, never by guessing
    // that those two turn identifiers are interchangeable.
    this.ensureSchema();
    const candidates = this.sql.exec<{ internal_turn_id: string }>(
      "SELECT internal_turn_id FROM managed2_tool_timing WHERE call_id = ? AND status IS NULL",
      context.callId,
    ).toArray();
    if (candidates.length === 1) {
      this.writePhase(candidates[0]!.internal_turn_id, context.callId, phase, durationMs);
    } else if (candidates.length === 0) {
      const pending = this.pending.get(context.callId) ?? [];
      pending.push({ phase, duration_ms: durationMs, count: 1 });
      this.pending.set(context.callId, pending);
    } // Concurrent provider reuse of a call ID is ambiguous; do not misattribute.
  }

  private writePhase(internal: string, callId: string, phase: string, durationMs: number): void {
    this.sql.exec(`INSERT INTO managed2_tool_phase (internal_turn_id, call_id, phase, duration_ms, count)
      VALUES (?, ?, ?, ?, 1)
      ON CONFLICT(internal_turn_id, call_id, phase)
      DO UPDATE SET duration_ms = duration_ms + excluded.duration_ms, count = count + 1`,
    internal, callId, phase, durationMs);
  }

  observe(internal: string, external: string, type: "tool.call" | "tool.result",
    payload: Record<string, unknown>, startedAt: number): void {
    const callId = payload.call_id;
    if (typeof callId !== "string") return;
    const elapsed = Math.max(0, Date.now() - startedAt);
    const schemaMs = this.ensureSchema();
    if (type === "tool.call") {
      this.sql.exec(`INSERT INTO managed2_tool_timing
        (internal_turn_id, call_id, external_turn_id, tool, started_at, started_ms)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(internal_turn_id, call_id) DO UPDATE SET
        external_turn_id = excluded.external_turn_id,
        tool = excluded.tool, started_at = excluded.started_at, started_ms = excluded.started_ms`,
      internal, callId, external, typeof payload.tool === "string" ? payload.tool : null,
      Date.now(), elapsed);
      if (schemaMs !== null) this.writePhase(internal, callId, "timing_schema_setup", schemaMs);
      const pending = this.pending.get(callId);
      if (pending) {
        this.pending.delete(callId);
        for (const row of pending) this.writePhase(internal, callId, row.phase, row.duration_ms);
      }
    } else {
      const duration = typeof payload.duration_ns === "number" ? payload.duration_ns / 1e6 : null;
      this.sql.exec(`INSERT INTO managed2_tool_timing
        (internal_turn_id, call_id, external_turn_id, tool, result_ms, duration_ms, status)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(internal_turn_id, call_id) DO UPDATE SET
        external_turn_id = excluded.external_turn_id,
        tool = excluded.tool, result_ms = excluded.result_ms,
        duration_ms = excluded.duration_ms, status = excluded.status`,
      internal, callId, external, typeof payload.tool === "string" ? payload.tool : null,
      elapsed, duration, typeof payload.status === "string" ? payload.status : null);
    }
  }

  list(external: string): unknown[] {
    if (this.schema === "unknown") {
      const exists = this.sql.exec<{ name: string }>(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'managed2_tool_timing'",
      ).toArray().length > 0;
      this.schema = exists ? "ready" : "absent";
    }
    if (this.schema === "absent") return [];
    return this.sql.exec<ToolRow>(`SELECT internal_turn_id, call_id, tool, started_at, started_ms,
      result_ms, duration_ms, status FROM managed2_tool_timing
      WHERE external_turn_id = ? ORDER BY started_ms, call_id`, external).toArray().map(row => {
      const phases = this.sql.exec<PhaseRow>(`SELECT phase, duration_ms, count FROM managed2_tool_phase
        WHERE internal_turn_id = ? AND call_id = ? ORDER BY phase`,
      row.internal_turn_id, row.call_id).toArray();
      return { call_id: row.call_id, tool: row.tool, clock: "io_gated", started_at: row.started_at,
        started_ms: row.started_ms, result_ms: row.result_ms,
        duration_ms: row.duration_ms, status: row.status,
        phases: Object.fromEntries(phases.map(({ phase, duration_ms, count }) =>
          [phase, { duration_ms, count }])) };
    });
  }
}
