import type {
  AgentEvent,
  EventWatcher,
  WatchEventsOptions,
} from "nanocodex";

type AgentEventSource = Readonly<{
  events: Readonly<{
    watch(options?: WatchEventsOptions): EventWatcher;
  }>;
}>;

type InternalEventListener = (
  event: AgentEvent,
  encodedLength?: number,
  encodedEvent?: string,
  agentId?: number,
) => void;

const REPLAY_EVENTS = new Set([
  "assistant.message",
  "reasoning.summary.delta",
  "run.started",
  "run.steered",
  "run.error",
  "run.completed",
  "run.failed",
  "tool.call",
  "tool.result",
  "model.call.started",
  "model.call.completed",
  "model.call.failed",
  "model.compaction.started",
  "model.compaction.completed",
  "model.compaction.failed",
]);

type ManagedAgentEventListeners = Readonly<{
  replay(event: AgentEvent, agentId: number | undefined): void;
  observe(event: AgentEvent): void;
}>;

/** Splits the Rust-owned agent family into replay state and transport telemetry. */
export function watchManagedAgentFamilyEvents(
  agent: AgentEventSource,
  listeners: ManagedAgentEventListeners,
): EventWatcher {
  const events = agent.events.watch({ includeAllSessions: true });
  const onEvent = events.onEvent as unknown as (
    listener: InternalEventListener,
  ) => () => void;
  onEvent((event, _encodedLength, _encodedEvent, agentId) => {
    // Provider frames and physical transport lifecycle are telemetry, not
    // replay state. In particular api.event repeats full requests and
    // cumulative response bodies already represented by normalized events.
    if (REPLAY_EVENTS.has(event.type)) {
      listeners.replay(event, agentId);
      return;
    }
    // Raw frames can contain prompts, tool schemas, and cumulative response
    // bodies. Cloudflare traces retain the request path; never copy payloads
    // into either replay storage or application logs.
    if (event.type !== "api.event" && event.type !== "assistant.delta") {
      listeners.observe(event);
    }
  });
  return events;
}
