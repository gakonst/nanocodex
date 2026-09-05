import {
  type ToolContext,
} from "nanocodex/host";
import type { ConnectAgent } from "nanocodex/connect";
import type { Agent, AgentEvent, AgentEventWatcher } from "nanocodex-react/agent";
import { createConnectAgentSource } from "nanocodex-react/connect";
import { createConnectedAgent, type NanocodexConnection } from "./connect";
import {
  cleanupPrompt,
  createCleanupTool,
  visibleCleanupPrompt,
  type CleanupInput,
} from "./extension";

export interface PageAgentSession {
  agent: ConnectAgent;
  source: Agent;
  close(): Promise<void>;
}

export interface CreatePageAgentOptions {
  connection: NanocodexConnection;
  dispatch(input: CleanupInput, context: ToolContext): unknown | Promise<unknown>;
  signal?: AbortSignal;
}

export async function createPageAgent(options: CreatePageAgentOptions): Promise<PageAgentSession> {
  const agent = await createConnectedAgent(
    options.connection,
    [createCleanupTool(options.dispatch)],
    options.signal,
  );
  return {
    agent,
    source: createCleanupAgentSource(agent, options.connection.grant.visibility.conversationHistory),
    async close() {
      await agent.session.shutdown();
    },
  };
}

function createCleanupAgentSource(agent: ConnectAgent, history: boolean): Agent {
  const source = createConnectAgentSource(agent, { history });
  return Object.freeze({
    sessionId: source.sessionId,
    events: Object.freeze({
      watch: () => sanitizeWatcher(source.events.watch()),
    }),
    turn: Object.freeze({
      prompt: ({ input }: Readonly<{ input: string }>) => source.turn.prompt({ input: cleanupPrompt(input) }),
    }),
  });
}

function sanitizeWatcher(watcher: AgentEventWatcher): AgentEventWatcher {
  return Object.freeze({
    onEvent(listener) {
      return watcher.onEvent((event) => listener(sanitizeEvent(event)));
    },
    ...(watcher.onHistory ? {
      onHistory(listener: (events: readonly AgentEvent[]) => void) {
        return watcher.onHistory!((events) => listener(events.map(sanitizeEvent)));
      },
    } : {}),
    ...(watcher.loadOlder ? { loadOlder: () => watcher.loadOlder!() } : {}),
    off: () => watcher.off(),
  });
}

function sanitizeEvent(event: AgentEvent): AgentEvent {
  if (event.type !== "managed.prompt" || typeof event.payload.text !== "string") return event;
  return Object.freeze({
    ...event,
    payload: Object.freeze({
      ...event.payload,
      text: visibleCleanupPrompt(event.payload.text),
    }),
  });
}
