import { describe, expect, it, vi } from "vitest";

import type {
  AgentEvent,
  EventWatcher,
  WatchEventsOptions,
} from "nanocodex";
import { watchManagedAgentFamilyEvents } from "../src/agent-event-watcher";

describe("managed agent event watcher", () => {
  it("projects replayable root and Rust-spawned child events without raw provider frames", () => {
    let subscribed: ((
      event: AgentEvent,
      encodedLength?: number,
      encodedEvent?: string,
      agentId?: number,
    ) => void) | undefined;
    const off = vi.fn();
    const watcher = {
      onEvent(listener: (event: AgentEvent) => void) {
        subscribed = listener as typeof subscribed;
        return vi.fn();
      },
      off,
      async *[Symbol.asyncIterator]() {},
    } satisfies EventWatcher;
    const watch = vi.fn((_options?: WatchEventsOptions) => watcher);
    const replayed: Array<{ event: AgentEvent; agentId: number | undefined }> = [];
    const observed: AgentEvent[] = [];

    expect(watchManagedAgentFamilyEvents(
      { events: { watch } },
      {
        replay(event, agentId) {
          replayed.push({ event, agentId });
        },
        observe(event) {
          observed.push(event);
        },
      },
    )).toBe(watcher);
    expect(watch).toHaveBeenCalledWith({ includeAllSessions: true });

    const root = agentEvent("root-session", 1, "run.started");
    const child = agentEvent("child-session", 1, "tool.call");
    subscribed!(agentEvent("root-session", 2, "assistant.delta"));
    for (const type of [
      "api.event",
      "model.warmup.started",
      "model.warmup.completed",
      "model.warmup.failed",
      "model.attempt.started",
      "model.attempt.failed",
      "model.attempt.retrying",
      "model.connection.started",
      "model.connection.completed",
      "model.connection.failed",
    ]) {
      const rootTransport = agentEvent("root-session", 2, type);
      const childTransport = agentEvent("child-session", 2, type);
      subscribed!(rootTransport, undefined, undefined, undefined);
      subscribed!(childTransport, undefined, undefined, 1);
    }
    subscribed!(root, undefined, undefined, undefined);
    subscribed!(child, undefined, undefined, 1);
    subscribed!(agentEvent("root-session", 3, "future.transport"));

    expect(replayed).toEqual([
      { event: root, agentId: undefined },
      { event: child, agentId: 1 },
    ]);
    expect(observed.map((event) => event.type)).toEqual([
      "model.warmup.started",
      "model.warmup.started",
      "model.warmup.completed",
      "model.warmup.completed",
      "model.warmup.failed",
      "model.warmup.failed",
      "model.attempt.started",
      "model.attempt.started",
      "model.attempt.failed",
      "model.attempt.failed",
      "model.attempt.retrying",
      "model.attempt.retrying",
      "model.connection.started",
      "model.connection.started",
      "model.connection.completed",
      "model.connection.completed",
      "model.connection.failed",
      "model.connection.failed",
      "future.transport",
    ]);
  });
});

function agentEvent(requestId: string, seq: number, type: string): AgentEvent {
  return {
    protocol_version: 1,
    request_id: requestId,
    seq,
    type,
    payload: {},
  };
}
