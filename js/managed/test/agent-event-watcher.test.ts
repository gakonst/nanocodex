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
    const rootDelta = {
      ...agentEvent("root-session", 3, "assistant.delta"),
      payload: { model_call_index: 0, item_id: "answer", phase: "final_answer", text: "1, " },
    };
    const nextDelta = {
      ...rootDelta,
      seq: 4,
      payload: { ...rootDelta.payload, text: "2, 3" },
    };
    const childDelta = {
      ...agentEvent("child-session", 3, "assistant.delta"),
      payload: { model_call_index: 0, text: "Working" },
    };
    subscribed!(rootDelta);
    subscribed!(nextDelta);
    subscribed!(childDelta, undefined, undefined, 1);
    // Chunks must reach replay/broadcast while the answer is still incomplete.
    expect(replayed.slice(2)).toEqual([
      { event: rootDelta, agentId: undefined },
      { event: nextDelta, agentId: undefined },
      { event: childDelta, agentId: 1 },
    ]);
    const message = {
      ...agentEvent("root-session", 5, "assistant.message"),
      payload: { ...rootDelta.payload, text: "1, 2, 3" },
    };
    subscribed!(message);
    const accepted = { ...agentEvent("root-session", 6, "input.accepted"), payload: { input: "private submitted text", kind: "steer", item_id: "input-1" } };
    subscribed!(accepted);
    subscribed!(agentEvent("root-session", 3, "future.transport"));

    expect(replayed).toEqual([
      { event: root, agentId: undefined },
      { event: child, agentId: 1 },
      { event: rootDelta, agentId: undefined },
      { event: nextDelta, agentId: undefined },
      { event: childDelta, agentId: 1 },
      { event: message, agentId: undefined },
      { event: accepted, agentId: undefined },
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

it("keeps compaction retries visible to hosted clients without retaining provider errors", () => {
  let deliver!: (event: AgentEvent, bytes?: number, encoded?: string, agentId?: number) => void;
  const replayed: Array<{ event: AgentEvent; agentId: number | undefined }> = [];
  const observed: AgentEvent[] = [];
  watchManagedAgentFamilyEvents({ events: { watch: () => ({
    onEvent(listener: (event: AgentEvent) => void) { deliver = listener; return () => {}; },
    off() {}, async *[Symbol.asyncIterator]() {},
  }) } }, {
    replay: (event, agentId) => replayed.push({ event, agentId }),
    observe: event => observed.push(event),
  });
  const started = agentEvent("root", 1, "model.compaction.started");
  const retry = { ...agentEvent("root", 2, "model.attempt.retrying"), payload: {
    phase: "compaction", attempt: 1, next_attempt: 2, max_attempts: 5,
    delay_ns: 200000000, opens_new_socket: true,
    error: "provider-private-detail", unknown: "provider-private-detail",
  } };
  const reconnect = { ...agentEvent("root", 3, "model.connection.started"), payload: {
    purpose: "reconnect", connection_generation: 2, websocket_url: "provider-private-detail",
  } };
  const connected = { ...agentEvent("root", 4, "model.connection.completed"), payload: {
    connection_generation: 2, request_id: "provider-private-detail",
  } };
  deliver(started, undefined, undefined, 7);
  deliver(retry, undefined, undefined, 7);
  deliver(reconnect, undefined, undefined, 7);
  deliver(connected, undefined, undefined, 7);
  expect(replayed.map(({ event }) => event.type)).toEqual([
    "model.compaction.started", "model.attempt.retrying", "model.connection.started", "model.connection.completed",
  ]);
  expect(replayed.every(({ agentId }) => agentId === 7)).toBe(true);
  expect(replayed[1].event.payload).toMatchObject({ delay_ns: 200000000, error: expect.any(String) });
  expect(replayed[2].event.payload).toMatchObject({ purpose: "reconnect" });
  expect(JSON.stringify(replayed)).not.toContain("provider-private-detail");
  expect(observed).toEqual([retry, reconnect, connected]);
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
