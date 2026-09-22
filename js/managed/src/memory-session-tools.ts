import type { NamedTool, ToolContext } from "nanocodex";
import {
  FIND_SESSIONS_TOOL_DESCRIPTION,
  READ_SESSION_TOOL_DESCRIPTION,
  findSessionsToolInputSchema,
  groupHistoryCitations,
  parseHistoryFindSessionsInput,
  parseHistoryReadSessionInput,
  projectFindSessionsToolResult,
  projectReadSessionToolResult,
  readSessionToolInputSchema,
  type HistoryCitation,
  type HistoryFindSessionsInput,
  type HistoryFindSessionsResponse,
  type HistoryReadSessionInput,
  type HistoryReadSessionResponse,
} from "nanocodex-tools/session";

export type MemorySessionToolCapability = "history:read";

export type MemorySessionToolOptions = Readonly<{
  findSessions(input: HistoryFindSessionsInput): Promise<HistoryFindSessionsResponse>;
  readSession(input: HistoryReadSessionInput): Promise<HistoryReadSessionResponse>;
  requireCapability(capability: MemorySessionToolCapability, context: ToolContext): void;
  recordCitations(citations: readonly HistoryCitation[]): void;
}>;

/**
 * Composes platform-neutral contracts with the managed host's account-scoped
 * persistence and active-turn authorization callbacks.
 */
export function memorySessionTools(options: MemorySessionToolOptions): readonly NamedTool[] {
  return [
    ...["find_session", "find_sessions"].map((name): NamedTool => ({
      name,
      description: FIND_SESSIONS_TOOL_DESCRIPTION,
      parameters: findSessionsToolInputSchema(),
      handler: async (input: unknown, context: ToolContext) => {
        options.requireCapability("history:read", context);
        const parsed = parseHistoryFindSessionsInput(input);
        const result = projectFindSessionsToolResult(
          await options.findSessions(parsed),
          parsed.limit,
        );
        context.signal.throwIfAborted();
        options.recordCitations(groupHistoryCitations(result.sessions.map((session) => ({
          thread_id: session.session_id,
          title: session.title,
          turn_id: session.turn_id,
          cursor: session.cursor,
        }))));
        return result;
      },
    })),
    {
      name: "read_session",
      description: READ_SESSION_TOOL_DESCRIPTION,
      parameters: readSessionToolInputSchema(),
      handler: async (input: unknown, context: ToolContext) => {
        options.requireCapability("history:read", context);
        const result = projectReadSessionToolResult(
          await options.readSession(parseHistoryReadSessionInput(input)),
        );
        context.signal.throwIfAborted();
        options.recordCitations(groupHistoryCitations(result.turns.map((turn) => ({
          thread_id: turn.session_id,
          title: turn.title,
          turn_id: turn.turn_id,
          cursor: turn.cursor,
        }))));
        return result;
      },
    },
  ];
}
