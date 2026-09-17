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
  // Clients need chunks before assistant.message completes the response.
  "assistant.delta",
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
    // Full provider frames remain telemetry. In particular api.event repeats
    // requests and cumulative bodies already represented by normalized events.
    if (REPLAY_EVENTS.has(event.type)) {
      listeners.replay(event, agentId);
      return;
    }
    const progress = transportProgress(event);
    if (progress) listeners.replay(progress, agentId);
    // Raw frames can contain prompts, tool schemas, and cumulative response
    // bodies. Cloudflare traces retain the request path; never copy payloads
    // into either replay storage or application logs.
    if (event.type !== "api.event") {
      listeners.observe(event);
    }
  });
  return events;
}

/** Client status uses a bounded projection, never raw provider errors or URLs. */
function transportProgress(event: AgentEvent): AgentEvent | undefined {
  const p = event.payload;
  const payload: Record<string, unknown> = {};
  if (event.type === "model.attempt.retrying") {
    if (!Number.isSafeInteger(p.delay_ns) || Number(p.delay_ns) < 0) return;
    payload.delay_ns = p.delay_ns;
    payload.error = "The model request is being retried.";
  } else if (event.type === "model.connection.started") {
    if (typeof p.purpose !== "string" || !["initial", "warmup_fallback", "reconnect"].includes(p.purpose)) return;
    payload.purpose = p.purpose;
  } else if (event.type === "model.connection.completed") {
    if (!Number.isSafeInteger(p.connection_generation) || Number(p.connection_generation) < 0) return;
  } else {
    return;
  }
  if (typeof p.phase === "string" && ["generation", "compaction", "warmup"].includes(p.phase)) payload.phase = p.phase;
  for (const key of ["attempt", "next_attempt", "max_attempts", "connection_generation", "model_call_index"]) {
    if (Number.isSafeInteger(p[key]) && Number(p[key]) >= 0) payload[key] = p[key];
  }
  return { protocol_version: event.protocol_version, request_id: event.request_id,
    seq: event.seq, type: event.type, payload };
}
