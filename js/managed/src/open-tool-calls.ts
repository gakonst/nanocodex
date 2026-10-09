import type { AgentEvent } from "nanocodex";
import type { DurableEvent } from "./durable-events";

/** Result text for a call whose runtime was lost before it could report. */
export const INTERRUPTED_TOOL_CALL_ERROR =
  "The session runtime restarted before this tool call reported, and the recovered run finished without it, so its outcome is unknown. Inspect external state before retrying any effect.";

type EventMessage = Readonly<{ type: "event"; event: AgentEvent; agent_id?: number }>;
type OpenCall = {
  agent_id: number; call_id: string; tool: string; request_id: string; protocol_version: number;
  seq: number; turn_id: string | null; runtime_turn_id: string | null; model_call_index: number | null;
};

/**
 * Durable index of child-subagent tool.call events that still lack a
 * tool.result, tagged with the isolate generation that published them.
 *
 * A child's Code Mode cell and its nested calls are awaited only by the
 * runtime in the isolate that started them. After isolate loss a restored
 * child resumes from its task-tree checkpoint with new call IDs (its restored
 * input names the interrupted call as outcome unknown), so the earlier calls
 * can never report. A previous-generation call is closed once the same child's
 * run in the current isolate finishes (run.completed or run.failed) without
 * re-emitting it; a re-emitted ID is re-tagged and settles normally. The
 * result is recorded right after that run terminal and keeps the call's
 * original turn, which may already be terminal when the child outlived it.
 *
 * Root calls are not indexed. Root recovery replays the same call IDs, and a
 * root run.failed can be retryable: a later attempt of the same turn replays
 * and settles the call, so closing it at the failed run would give one call
 * two terminal results.
 *
 * Not covered, and left open rather than falsely terminal: root calls a lost
 * isolate abandoned without replay; calls of a child that never runs again;
 * calls of a runtime torn down and reopened within the same isolate (only
 * isolate loss starts a new generation). Forward-only: calls published before
 * this index existed are not backfilled from retained history.
 */
export class OpenToolCalls {
  readonly #storage: DurableObjectStorage;
  readonly #generation: string;

  constructor(storage: DurableObjectStorage, generation: string = crypto.randomUUID()) {
    this.#storage = storage;
    this.#generation = generation;
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_open_tool_calls (
      agent_id INTEGER NOT NULL, call_id TEXT NOT NULL, tool TEXT NOT NULL,
      request_id TEXT NOT NULL, protocol_version INTEGER NOT NULL, seq INTEGER NOT NULL,
      turn_id TEXT, runtime_turn_id TEXT, model_call_index INTEGER, cursor INTEGER NOT NULL,
      generation TEXT NOT NULL, PRIMARY KEY (agent_id, call_id)
    )`);
  }

  /**
   * Called for every appended durable event, inside its append. Returns the
   * child whose run just finished, whose abandoned calls can now be settled.
   */
  observe(event: DurableEvent<{ type: string }>): number | undefined {
    const message = event.message as Partial<EventMessage>;
    if (message.type !== "event" || !message.event) return;
    // Root events carry no agent_id; see the class comment.
    if (!Number.isSafeInteger(message.agent_id)) return;
    const agent = message.agent_id!;
    const { type, payload } = message.event;
    if (type === "run.completed" || type === "run.failed") return agent;
    if (type !== "tool.call" && type !== "tool.result") return;
    const callId = payload?.call_id;
    if (typeof callId !== "string" || !callId) return;
    if (type === "tool.result") {
      this.#storage.sql.exec("DELETE FROM managed_open_tool_calls WHERE agent_id = ? AND call_id = ?", agent, callId);
      return;
    }
    const { protocol_version: version, request_id: requestId, seq } = message.event;
    this.#storage.sql.exec(
      `INSERT OR REPLACE INTO managed_open_tool_calls (agent_id, call_id, tool, request_id,
        protocol_version, seq, turn_id, runtime_turn_id, model_call_index, cursor, generation)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      agent, callId, typeof payload.tool === "string" ? payload.tool : "unknown",
      typeof requestId === "string" ? requestId : "",
      Number.isSafeInteger(version) ? version : 1, Number.isSafeInteger(seq) ? seq : 0,
      event.turn_id, typeof payload.turn_id === "string" ? payload.turn_id : null,
      Number.isSafeInteger(payload.model_call_index) ? payload.model_call_index as number : null,
      Number(event.cursor), this.#generation,
    );
  }

  /**
   * Terminal results for the child's calls that a previous isolate left open,
   * newest first so nested Code Mode calls settle before their cell. Appending
   * them removes them from the index.
   */
  abandoned(agent: number): { message: EventMessage; turnId: string | null }[] {
    return this.#storage.sql.exec<OpenCall>(
      `SELECT agent_id, call_id, tool, request_id, protocol_version, seq, turn_id,
        runtime_turn_id, model_call_index FROM managed_open_tool_calls
       WHERE agent_id = ? AND generation != ? ORDER BY cursor DESC`,
      agent, this.#generation,
    ).toArray().map(call => ({
      turnId: call.turn_id,
      message: {
        type: "event",
        event: {
          protocol_version: call.protocol_version,
          request_id: call.request_id,
          seq: call.seq,
          type: "tool.result",
          payload: {
            call_id: call.call_id,
            tool: call.tool,
            status: "failed",
            result: INTERRUPTED_TOOL_CALL_ERROR,
            structured_result: { code: "TOOL_CALL_INTERRUPTED", error: INTERRUPTED_TOOL_CALL_ERROR, outcome: "unknown" },
            ...(call.runtime_turn_id === null ? {} : { turn_id: call.runtime_turn_id }),
            ...(call.model_call_index === null ? {} : { model_call_index: call.model_call_index }),
          },
        },
        agent_id: call.agent_id,
      },
    }));
  }

  clear(): void {
    this.#storage.sql.exec("DELETE FROM managed_open_tool_calls");
  }
}
