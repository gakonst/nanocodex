import assert from "node:assert/strict";
import { test } from "node:test";

import { createExampleAgentController } from "../src/agentController.ts";
import {
  appendRetainedEvents,
  summarizeEventBatch,
} from "../src/eventBatch.ts";

test("the example Worker owns follow-ons, optional events, results, and payment state", async () => {
  const harness = new Harness();
  const messages: any[] = [];
  const controller = createExampleAgentController({
    createAgent: async () => ({
      agent: harness.createAgent("root") as any,
      payment: {
        rootAddress: "0xroot",
        accessKeyAddress: "0xaccess",
        channelId: "channel-1",
        cumulative: () => "123",
      },
    }),
    postMessage: (message) => messages.push(message),
  });

  await controller.handle({
    type: "start",
    transport: "mpp",
    thinking: "high",
    reasoningMode: "pro",
  });
  assert.deepEqual(harness.watchOptions, [undefined]);
  assert.deepEqual(messages.shift(), {
    type: "ready",
    transport: "mpp",
    rootAddress: "0xroot",
    accessKeyAddress: "0xaccess",
    channelId: "channel-1",
  });

  harness.emit(event(1, "run.started"));
  assert.deepEqual(messages.shift(), {
    type: "event",
    event: event(1, "run.started"),
  });

  await controller.handle({ type: "prompt", id: 1, prompt: "first" });
  await controller.handle({ type: "prompt", id: 2, prompt: "follow on" });
  assert.deepEqual(
    harness.turns.map((turn) => turn.input),
    ["first", "follow on"],
  );
  harness.turns[1]!.complete("second result");
  harness.turns[0]!.complete("first result");
  await settle();

  assert.deepEqual(messages, [
    {
      type: "result",
      id: 1,
      message: "first result",
      payment: { channelId: "channel-1", cumulative: "123" },
    },
    {
      type: "result",
      id: 2,
      message: "second result",
      payment: { channelId: "channel-1", cumulative: "123" },
    },
  ]);
  assert.equal(harness.turns[0]?.disposed, 1);
  assert.equal(harness.turns[1]?.disposed, 1);
  assert.equal(harness.turns[0]?.resultDisposed, 1);
  assert.equal(harness.turns[1]?.resultDisposed, 1);
  await controller.dispose();
  assert.equal(harness.agents[0]?.disposed, 1);
  assert.equal(harness.watchOffs, 1);
});

test("prompt failures are typed messages and always release accepted Turns", async () => {
  const harness = new Harness();
  const messages: any[] = [];
  const controller = createExampleAgentController({
    createAgent: async () => ({
      agent: harness.createAgent("root") as any,
    }),
    postMessage: (message) => messages.push(message),
  });

  await controller.handle({ type: "prompt", id: 1, prompt: "early" });
  assert.deepEqual(messages.shift(), {
    type: "error",
    id: 1,
    message: "Start the agent first.",
  });
  await controller.handle({
    type: "start",
    transport: "openai",
    thinking: "high",
  });
  messages.length = 0;

  harness.promptError = new Error("acceptance failed");
  await controller.handle({ type: "prompt", id: 2, prompt: "rejected" });
  assert.deepEqual(messages.shift(), {
    type: "error",
    id: 2,
    message: "acceptance failed",
  });

  harness.resultError = new Error("result threw");
  await controller.handle({ type: "prompt", id: 3, prompt: "bad result" });
  await settle();
  assert.deepEqual(messages.shift(), {
    type: "error",
    id: 3,
    message: "result threw",
  });
  assert.equal(harness.turns[0]?.disposed, 1);

  await controller.handle({ type: "prompt", id: 4, prompt: "failed turn" });
  harness.turns[1]!.fail(new Error("model failed"));
  await settle();
  assert.deepEqual(messages.shift(), {
    type: "error",
    id: 4,
    message: "model failed",
  });
  assert.equal(harness.turns[1]?.disposed, 1);
  await controller.dispose();
});

test("restart releases active Turns and stale event listeners exactly once", async () => {
  const harness = new Harness();
  const messages: any[] = [];
  let generation = 0;
  const controller = createExampleAgentController({
    createAgent: async () => ({
      agent: harness.createAgent(`root-${++generation}`) as any,
    }),
    postMessage: (message) => messages.push(message),
  });
  const start = {
    type: "start" as const,
    transport: "openai" as const,
    thinking: "high" as const,
  };

  await controller.handle(start);
  const staleListener = harness.listeners[0];
  await controller.handle({ type: "prompt", id: 1, prompt: "active" });
  messages.length = 0;
  await controller.handle(start);

  assert.equal(harness.agents[0]?.disposed, 1);
  assert.equal(harness.turns[0]?.cancelled, 1);
  assert.equal(harness.turns[0]?.disposed, 1);
  staleListener?.(event(9, "assistant.delta", { text: "stale" }));
  assert.deepEqual(messages, [{
    type: "ready",
    transport: "openai",
  }]);

  harness.turns[0]!.complete("late");
  await settle();
  assert.equal(harness.turns[0]?.disposed, 1);
  assert.equal(harness.turns[0]?.resultDisposed, 1);
  assert.deepEqual(messages, [{ type: "ready", transport: "openai" }]);
  await controller.dispose();
});

test("frame reduction preserves ordered final and terminal semantics", () => {
  const summary = summarizeEventBatch([
    event(1, "run.started"),
    event(2, "assistant.delta", { text: "draft" }),
    event(3, "assistant.message", { text: "final" }),
    event(4, "reasoning.summary.delta", { text: "checked" }),
    event(5, "run.error", { message: "warning" }),
    event(6, "run.completed", { duration_ms: 37 }),
  ] as any, 100, 200);

  assert.deepEqual(summary, {
    assistant: { mode: "replace", text: "final" },
    elapsedMs: 37,
    errors: ["warning"],
    reasoning: "checked",
    status: "completed",
  });
});

test("frame reduction deduplicates repeated errors and honors zero retention", () => {
  const summary = summarizeEventBatch([
    event(1, "run.error", { message: "connection lost" }),
    event(2, "run.error", { message: "connection lost" }),
  ] as any, 0, 0);

  assert.deepEqual(summary.errors, ["connection lost"]);
  assert.deepEqual(
    appendRetainedEvents([event(0, "run.started")] as any, [] as any, 0),
    [],
  );
});

function event(
  seq: number,
  type: string,
  payload: Record<string, unknown> = {},
) {
  return {
    protocol_version: 1,
    request_id: "root",
    seq,
    type,
    payload,
  };
}

function settle() {
  return new Promise<void>((resolve) => setImmediate(resolve));
}

class Harness {
  agents: FakeAgent[] = [];
  turns: FakeTurn[] = [];
  listeners: Array<(event: any) => void> = [];
  watchOptions: unknown[] = [];
  watchOffs = 0;
  promptError?: Error;
  resultError?: Error;

  createAgent(sessionId: string) {
    const agent = new FakeAgent(this, sessionId);
    this.agents.push(agent);
    return agent;
  }

  emit(value: any) {
    this.listeners.at(-1)?.(value);
  }
}

class FakeAgent {
  disposed = 0;
  readonly sessionId: string;
  private readonly harness: Harness;
  turn;
  events;
  session;

  constructor(harness: Harness, sessionId: string) {
    this.harness = harness;
    this.sessionId = sessionId;
    this.turn = {
      prompt: ({ input }: { input: string }) => {
        if (this.harness.promptError) {
          const error = this.harness.promptError;
          this.harness.promptError = undefined;
          throw error;
        }
        const turn = new FakeTurn(input, this.harness.resultError);
        this.harness.resultError = undefined;
        this.harness.turns.push(turn);
        return turn;
      },
    };
    this.session = {
      shutdown: async () => {
        this.dispose();
      },
    };
    this.events = {
      watch: (options?: unknown) => {
        this.harness.watchOptions.push(options);
        return {
          onEvent: (listener: (event: any) => void) => {
            this.harness.listeners.push(listener);
            return () => {};
          },
          off: () => {
            this.harness.watchOffs += 1;
          },
        };
      },
    };
  }

  dispose() {
    this.disposed += 1;
  }
}

class FakeTurn {
  cancelled = 0;
  disposed = 0;
  resultDisposed = 0;
  readonly input: string;
  private readonly resultError?: Error;
  private resolve!: (value: FakeTurnResult) => void;
  private reject!: (error: unknown) => void;
  private readonly completion: Promise<FakeTurnResult>;

  constructor(input: string, resultError?: Error) {
    this.input = input;
    this.resultError = resultError;
    this.completion = new Promise((resolve, reject) => {
      this.resolve = resolve;
      this.reject = reject;
    });
  }

  result() {
    if (this.resultError) throw this.resultError;
    return this.completion;
  }

  complete(value: string) {
    this.resolve(turnResult(value, () => {
      this.resultDisposed += 1;
    }));
  }

  fail(error: unknown) {
    this.reject(error);
  }

  async cancel() {
    this.cancelled += 1;
  }

  dispose() {
    this.disposed += 1;
  }
}

type FakeTurnResult = ReturnType<typeof turnResult>;

function turnResult(finalMessage: string, dispose = () => {}) {
  return {
    finalMessage,
    dispose,
  } as const;
}
