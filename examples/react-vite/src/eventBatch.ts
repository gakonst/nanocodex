import type { AgentEvent } from "nanocodex";

export type EventBatchSummary = {
  assistant: { mode: "append" | "replace"; text: string } | undefined;
  elapsedMs: number | undefined;
  errors: string[];
  reasoning: string;
  status: "running" | "completed" | "failed" | undefined;
};

/** Reduces one browser frame of ordered events into one React update. */
export function summarizeEventBatch(
  events: readonly AgentEvent[],
  startedAt: number,
  now: number,
): EventBatchSummary {
  let assistantMode: "append" | "replace" = "append";
  const assistant: string[] = [];
  const reasoning: string[] = [];
  const errors: string[] = [];
  let status: EventBatchSummary["status"];
  let elapsedMs: number | undefined;

  for (const event of events) {
    if (event.type === "run.started") {
      status = "running";
    } else if (event.type === "assistant.delta") {
      assistant.push(payloadText(event));
    } else if (event.type === "assistant.message") {
      assistantMode = "replace";
      assistant.length = 0;
      assistant.push(payloadText(event));
    } else if (event.type === "reasoning.summary.delta") {
      reasoning.push(payloadText(event));
    } else if (event.type === "run.completed") {
      status = "completed";
      elapsedMs = payloadNumber(event.payload, "duration_ms") ?? now - startedAt;
    } else if (event.type === "run.failed") {
      status = "failed";
      elapsedMs = payloadNumber(event.payload, "duration_ms") ?? now - startedAt;
    } else if (event.type === "run.error") {
      const message =
        payloadString(event.payload, "message") ?? "The run failed.";
      if (errors.at(-1) !== message) errors.push(message);
    }
  }

  return {
    assistant: assistant.length
      ? { mode: assistantMode, text: assistant.join("") }
      : undefined,
    elapsedMs,
    errors,
    reasoning: reasoning.join(""),
    status,
  };
}

/** Retains the newest events without copying once per event in a burst. */
export function appendRetainedEvents(
  current: readonly AgentEvent[],
  incoming: readonly AgentEvent[],
  limit = 500,
): AgentEvent[] {
  if (limit <= 0) return [];
  if (incoming.length >= limit) return incoming.slice(-limit);
  const keep = Math.max(0, limit - incoming.length);
  return [...current.slice(-keep), ...incoming];
}

function payloadText(event: AgentEvent): string {
  return payloadString(event.payload, "text") ?? "";
}

function payloadString(
  payload: Record<string, unknown>,
  key: string,
): string | undefined {
  return typeof payload[key] === "string" ? payload[key] : undefined;
}

function payloadNumber(
  payload: Record<string, unknown>,
  key: string,
): number | undefined {
  return typeof payload[key] === "number" ? payload[key] : undefined;
}
