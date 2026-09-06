import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import { test } from "node:test";
import { gunzipSync } from "node:zlib";
import WebSocket from "ws";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";
import { startResponsesServer, messageReader, sendWarmup, sendFinal } from "./support/responses.mjs";

import { Actions } from "../index.mjs";
import { Agent as HostAgent, Transport as HostTransport } from "../host/index.mjs";
import {
  createWorkerAgent,
  installWorkerAgentRuntime,
} from "../browser/WorkerAgent.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import {
  createAgentClient,
  defineRuntime,
} from "../internal.mjs";
import { Agent as NodeAgent, Transport as NodeTransport } from "../node/index.mjs";
import { createCodeRuntime } from "../runtime/code-runtime.mjs";

const LIMITS = Object.freeze({
  coldNodeAgentMs: 250,
  warmAgentP50Ms: 1.5,
  warmAgentP95Ms: 10,
  browserLinearMemoryBytes: 2_500_000,
  actionNanoseconds: 5_000,
  bufferedEventsMs: 50,
  codeModeMicroseconds: 250,
  workerResultEnvelopeBytes: 256,
});
const nodeTransport = NodeTransport.openAi({ apiKey: "performance-test" });
const browserTransport = HostTransport.openAi({
  apiKey: "performance-test",
  WebSocketImpl: class {},
});

test("Node reuses one compiled WASM instance and keeps warm agent creation sub-millisecond", async (context) => {
  const OriginalModule = WebAssembly.Module;
  const OriginalInstance = WebAssembly.Instance;
  let modules = 0;
  let instances = 0;
  WebAssembly.Module = class extends OriginalModule {
    constructor(...arguments_) {
      super(...arguments_);
      modules += 1;
    }
  };
  WebAssembly.Instance = class extends OriginalInstance {
    constructor(...arguments_) {
      super(...arguments_);
      instances += 1;
    }
  };
  try {
    const coldStarted = performance.now();
    const cold = await NodeAgent.create({ transport: nodeTransport });
    const coldMs = performance.now() - coldStarted;
    cold.dispose();

    const samples = [];
    for (let index = 0; index < 64; index += 1) {
      const started = performance.now();
      const agent = await NodeAgent.create({ transport: nodeTransport });
      samples.push(performance.now() - started);
      agent.dispose();
    }
    const p50 = percentile(samples, 0.5);
    const p95 = percentile(samples, 0.95);
    context.diagnostic(JSON.stringify({
      cold_ms: round(coldMs),
      module_compilations: modules,
      module_instantiations: instances,
      warm_p50_ms: round(p50),
      warm_p95_ms: round(p95),
    }));

    assert.equal(modules, 1);
    assert.equal(instances, 1);
    assert.ok(coldMs <= LIMITS.coldNodeAgentMs, `cold Node Agent.create took ${coldMs} ms`);
    assert.ok(p50 <= LIMITS.warmAgentP50Ms, `warm Node Agent.create p50 was ${p50} ms`);
    assert.ok(p95 <= LIMITS.warmAgentP95Ms, `warm Node Agent.create p95 was ${p95} ms`);
  } finally {
    WebAssembly.Module = OriginalModule;
    WebAssembly.Instance = OriginalInstance;
  }
});

test("a precompiled browser module instantiates once across isolated agents", async (context) => {
  const bytes = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const module = await WebAssembly.compile(bytes);
  const originalInstantiate = WebAssembly.instantiate;
  let instantiations = 0;
  WebAssembly.instantiate = (...arguments_) => {
    instantiations += 1;
    return originalInstantiate(...arguments_);
  };
  try {
    const coldStarted = performance.now();
    const cold = await HostAgent.create({
      transport: browserTransport,
      module,
    });
    const coldMs = performance.now() - coldStarted;
    const engine = await initializeBrowserEngine({ module });
    const coldLinearMemoryBytes = engine.memory.buffer.byteLength;
    cold.dispose();
    for (let index = 0; index < 16; index += 1) {
      const agent = await HostAgent.create({
        transport: browserTransport,
        module,
      });
      agent.dispose();
    }

    const samples = [];
    for (let index = 0; index < 64; index += 1) {
      const started = performance.now();
      const agent = await HostAgent.create({
        transport: browserTransport,
        module,
      });
      samples.push(performance.now() - started);
      agent.dispose();
    }
    const p50 = percentile(samples, 0.5);
    const p95 = percentile(samples, 0.95);
    const retainedLinearMemoryBytes = engine.memory.buffer.byteLength;
    context.diagnostic(JSON.stringify({
      cold_ms: round(coldMs),
      cold_linear_memory_bytes: coldLinearMemoryBytes,
      module_instantiations: instantiations,
      retained_linear_memory_bytes: retainedLinearMemoryBytes,
      warm_p50_ms: round(p50),
      warm_p95_ms: round(p95),
    }));

    assert.equal(instantiations, 1);
    assert.equal(retainedLinearMemoryBytes, coldLinearMemoryBytes);
    assert.ok(
      retainedLinearMemoryBytes <= LIMITS.browserLinearMemoryBytes,
      `browser WASM retained ${retainedLinearMemoryBytes} linear-memory bytes`,
    );
    assert.ok(coldMs <= LIMITS.coldNodeAgentMs, `cold browser Agent.create took ${coldMs} ms`);
    assert.ok(p50 <= LIMITS.warmAgentP50Ms, `warm browser Agent.create p50 was ${p50} ms`);
    assert.ok(p95 <= LIMITS.warmAgentP95Ms, `warm browser Agent.create p95 was ${p95} ms`);
  } finally {
    WebAssembly.instantiate = originalInstantiate;
  }
});

test("long durable histories preserve cold replay and cancellation results", {
  timeout: 180_000,
}, async (context) => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const engine = await initializeBrowserEngine({ module });
  const server = await startResponsesServer();
  context.after(() => server.close());
  const store = createMemoryDurabilityStore("long-memory-budget");
  const options = {
    module,
    transport: HostTransport.openAi({
      apiKey: "fixture", websocketUrl: server.url, WebSocketImpl: WebSocket, websocketWarmup: true,
    }),
    durability: store, durabilityId: "long-memory-budget", terminalReceiptRetention: 16,
    thinking: "low",
  };
  const agent = await HostAgent.create(options);
  context.after(() => agent.session.shutdown());
  const scenario = (async () => {
    const socket = await server.nextConnection();
    const reader = messageReader(socket);
    await reader.next();
    sendWarmup(socket, "warmup");
    for (let index = 0; index < 96; index += 1) {
      await reader.next();
      sendFinal(socket, `response-${index}`, `DONE_${index}`);
    }
  })();
  const input = Array.from({ length: 160 }, (_, index) =>
    `Synthetic record ${index}: durability preserves operation order, exact inputs, and committed results.`).join("\n");
  for (let index = 0; index < 96; index += 1) {
    const turn = agent.turn.prompt({ id: `turn-${index}`, input });
    const result = await turn.result();
    assert.equal(result.finalMessage, `DONE_${index}`);
    result.dispose();
    turn.dispose();
  }
  await scenario;
  const liveWasmBytes = engine.memory.buffer.byteLength;
  await agent.session.shutdown();
  const reopened = await HostAgent.create(options);
  const reopenedWasmBytes = engine.memory.buffer.byteLength;
  context.after(() => reopened.session.shutdown());
  const replay = reopened.turn.prompt({ id: "turn-95", input });
  const result = await replay.result();
  assert.equal(result.finalMessage, "DONE_95");
  result.dispose();
  replay.dispose();
  // Report WASM allocation without reserving an arbitrary fraction of the
  // Worker's shared JS/WASM memory limit as a separate pass/fail threshold.
  const wasmBytes = engine.memory.buffer.byteLength;
  const payloadBytes = Buffer.byteLength(store.load("long-memory-budget").payload);
  context.diagnostic(JSON.stringify({ long_thread_wasm_bytes: wasmBytes,
    live_wasm_bytes: liveWasmBytes, reopened_wasm_bytes: reopenedWasmBytes,
    durable_payload_bytes: payloadBytes, turns: 96, cold_replay: true }));
  assert.ok(payloadBytes < 4 * 1024 * 1024, `long thread persisted ${payloadBytes} bytes`);
  await reopened.session.shutdown();

  // Older deployments saved the same state as uncompressed JSON. They must
  // reopen without creating a second escaped full-state envelope in the host.
  const retained = store.load("long-memory-budget");
  const prefix = "nanocodex-durable-state-gzip-v1:";
  assert.ok(retained.payload.startsWith(prefix));
  const legacy = createMemoryDurabilityStore("legacy-memory-budget", {
    revision: retained.revision,
    payload: gunzipSync(Buffer.from(retained.payload.slice(prefix.length), "base64")).toString("utf8"),
  });
  const legacyAgent = await HostAgent.create({
    ...options, durability: legacy, durabilityId: "legacy-memory-budget",
  });
  context.after(() => legacyAgent.session.shutdown());
  const legacyTurn = legacyAgent.turn.prompt({ id: "turn-95", input });
  const legacyResult = await legacyTurn.result();
  assert.equal(legacyResult.finalMessage, "DONE_95");
  legacyResult.dispose();
  legacyTurn.dispose();
  const legacyWasmBytes = engine.memory.buffer.byteLength;
  context.diagnostic(JSON.stringify({ legacy_reopen_wasm_bytes: legacyWasmBytes }));
  for (let index = 0; index < 432; index += 1) {
    const cancelled = legacyAgent.turn.prompt({
      id: `cancel-${index}`, input: "Cancelled archive fixture.", cancelOnAdmission: true,
    });
    await assert.rejects(cancelled.result(), /cancel/i);
    cancelled.dispose();
  }
  const cancellationWasmBytes = engine.memory.buffer.byteLength;
  context.diagnostic(JSON.stringify({ cancellation_wasm_bytes: cancellationWasmBytes, cancellations: 432 }));
});

test("Worker completion keeps a large retained snapshot out of the eager crossover", async (context) => {
  const retainedText = "x".repeat(8 * 1024 * 1024);
  const encodedSnapshot = JSON.stringify({
    version: 1,
    model: "gpt-5.6-sol",
    lineage_id: "large-worker-result",
    prompt_cache_key: "large-worker-result",
    workspace: "/workspace",
    canonical_context: {},
    history: [{ type: "message", role: "assistant", content: retainedText }],
  });
  const stats = { resultReleases: 0, snapshotReads: 0, usageReads: 0 };
  const rawResult = {
    finalMessage: "done",
    snapshot() { stats.snapshotReads += 1; return encodedSnapshot; },
    usage() {
      stats.usageReads += 1;
      return JSON.stringify({
        input_tokens: 1,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 1,
        reasoning_output_tokens: 0,
        total_tokens: 2,
        estimated_cost: null,
        cost_status: "usage_not_reported",
      });
    },
    free() { stats.resultReleases += 1; },
  };
  const runtime = defineRuntime({
    // This is the simulated Worker isolate; the page-side Worker client owns
    // the same-session reservation in this process.
    reserveSessions: false,
    create: ({ sessionId = "large-worker-result" } = {}) => ({
      sessionId,
      prompt() {
        return {
          result: async () => rawResult,
          free() {},
        };
      },
      free() {},
    }),
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const worker = new CrossoverLoopbackWorker((options) => createAgentClient(runtime, options));
  const agent = await createWorkerAgent(
    { sessionId: "large-worker-result", harness: false },
    { worker },
  );
  const turn = agent.turn.prompt({ input: "measure the retained checkpoint" });
  const result = await turn.result();
  const completion = worker.outgoing.find((message) => message.value?.resultId);
  const completionEnvelopeBytes = new TextEncoder().encode(JSON.stringify(completion.value)).byteLength;
  const retainedSnapshotBytes = new TextEncoder().encode(encodedSnapshot).byteLength;

  assert.deepEqual(Object.keys(completion.value).sort(), ["finalMessage", "resultId"]);
  assert.deepEqual(stats, { resultReleases: 0, snapshotReads: 0, usageReads: 0 });
  assert.ok(
    completionEnvelopeBytes <= LIMITS.workerResultEnvelopeBytes,
    `Worker result envelope used ${completionEnvelopeBytes} bytes`,
  );

  const [snapshot, sameSnapshot] = await Promise.all([result.snapshot(), result.snapshot()]);
  assert.strictEqual(sameSnapshot, snapshot);
  assert.equal(snapshot.history[0].content.length, retainedText.length);
  assert.equal(stats.snapshotReads, 1);
  assert.equal(worker.incoming.filter((message) => message.method === "result.snapshot").length, 1);
  context.diagnostic(JSON.stringify({
    avoided_eager_crossover_bytes: retainedSnapshotBytes,
    completion_envelope_bytes: completionEnvelopeBytes,
    eager_snapshot_materializations: 0,
    retained_snapshot_bytes: retainedSnapshotBytes,
    snapshot_materializations_after_demand: stats.snapshotReads,
    snapshot_rpcs_after_concurrent_demand: 1,
  }));

  result.dispose();
  turn.dispose();
  agent.dispose();
  assert.equal(stats.resultReleases, 1);
  assert.equal(worker.terminated, 1);
});

test("JavaScript actions, event buffering, and Code Mode stay below binding-owned budgets", async (context) => {
  const subscriptions = new Set();
  const rawTurn = {
    async result() { return "done"; },
    free() {},
  };
  const rawAgent = {
    sessionId: "performance-session",
    prompt() { return rawTurn; },
    free() {},
  };
  const agent = await createAgentClient(defineRuntime({
    create: () => rawAgent,
    subscribe(listener) {
      subscriptions.add(listener);
      return () => subscriptions.delete(listener);
    },
    decorate: (client) => client.extend(Actions.agentActions()),
  }));

  const actionIterations = 50_000;
  // Collect preceding workloads outside each independent timing phase. GC
  // during the timed workload still counts toward its budget.
  globalThis.gc?.();
  const actionStarted = performance.now();
  for (let index = 0; index < actionIterations; index += 1) {
    agent.turn.prompt({ input: "measure wrapper overhead" }).dispose();
  }
  const actionNanoseconds = (
    (performance.now() - actionStarted) * 1_000_000 / actionIterations
  );

  const watch = agent.events.watch();
  const iterator = watch[Symbol.asyncIterator]();
  const eventCount = 4_096;
  // In particular, do not charge collection of 50,000 disposed prompt wrappers
  // to event buffering simply because V8 scheduled it at the next allocation.
  globalThis.gc?.();
  const eventsStarted = performance.now();
  for (let seq = 1; seq <= eventCount; seq += 1) {
    for (const listener of subscriptions) {
      listener({ request_id: agent.sessionId, seq, type: "api.event" });
    }
  }
  for (let seq = 1; seq <= eventCount; seq += 1) {
    assert.equal((await iterator.next()).value.seq, seq);
  }
  const bufferedEventsMs = performance.now() - eventsStarted;
  await iterator.return();
  watch.off();
  agent.dispose();

  const code = createCodeRuntime({
    increment: {
      description: "Increment an integer.",
      parameters: {
        type: "object",
        properties: { value: { type: "integer" } },
        required: ["value"],
      },
      handler: ({ value }) => value + 1,
    },
  });
  const codeIterations = 1_000;
  globalThis.gc?.();
  const codeStarted = performance.now();
  for (let index = 0; index < codeIterations; index += 1) {
    const result = JSON.parse(await code.executeCode(
      `text(await tools.increment({ value: ${index} }));`,
      "performance-session",
      `call-${index}`,
    ));
    assert.equal(result.success, true);
  }
  const codeModeMicroseconds = (
    (performance.now() - codeStarted) * 1_000 / codeIterations
  );
  context.diagnostic(JSON.stringify({
    action_ns_per_prompt: round(actionNanoseconds),
    buffered_events: eventCount,
    buffered_events_ms: round(bufferedEventsMs),
    code_mode_us_per_execution: round(codeModeMicroseconds),
  }));

  assert.ok(
    actionNanoseconds <= LIMITS.actionNanoseconds,
    `prompt action wrapper took ${actionNanoseconds} ns per call`,
  );
  assert.ok(
    bufferedEventsMs <= LIMITS.bufferedEventsMs,
    `${eventCount} buffered events took ${bufferedEventsMs} ms`,
  );
  assert.ok(
    codeModeMicroseconds <= LIMITS.codeModeMicroseconds,
    `Code Mode host took ${codeModeMicroseconds} µs per execution`,
  );
});

function percentile(values, quantile) {
  const ordered = values.toSorted((left, right) => left - right);
  return ordered[Math.min(ordered.length - 1, Math.floor(ordered.length * quantile))];
}

function round(value) {
  return Math.round(value * 1_000) / 1_000;
}

class CrossoverLoopbackWorker {
  constructor(createAgent) {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.incoming = [];
    this.outgoing = [];
    this.terminated = 0;
    this.scope = {
      onmessage: null,
      postMessage: (message, transfer) => {
        const cloned = transfer?.length
          ? structuredClone(message, { transfer })
          : structuredClone(message);
        this.outgoing.push(cloned);
        queueMicrotask(() => this.onmessage?.({ data: cloned }));
      },
    };
    this.runtime = installWorkerAgentRuntime(this.scope, { createAgent });
  }

  postMessage(message) {
    const cloned = structuredClone(message);
    this.incoming.push(cloned);
    queueMicrotask(() => this.scope.onmessage?.({ data: cloned }));
  }

  terminate() {
    if (this.terminated) return;
    this.terminated += 1;
    this.runtime.dispose();
  }
}
