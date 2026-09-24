import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import { test } from "node:test";
import WebSocket from "ws";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";
import { startResponsesServer, messageReader, sendWarmup, sendFinal } from "./support/responses.mjs";

import { Agent as HostAgent, Transport as HostTransport } from "../host/index.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import { Agent as NodeAgent, Transport as NodeTransport } from "../node/index.mjs";

const LIMITS = Object.freeze({
  coldNodeAgentMs: 250,
  warmAgentP50Ms: 1.5,
  warmAgentP95Ms: 10,
  browserLinearMemoryBytes: 2_500_000,
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
  assert.ok(payloadBytes < 32 * 1024, `long thread persisted ${payloadBytes} bytes`);
  await reopened.session.shutdown();

  const cancellationAgent = await HostAgent.create(options);
  context.after(() => cancellationAgent.session.shutdown());
  for (let index = 0; index < 432; index += 1) {
    const cancelled = cancellationAgent.turn.prompt({
      id: `cancel-${index}`, input: "Cancelled archive fixture.", cancelOnAdmission: true,
    });
    await assert.rejects(cancelled.result(), /cancel/i);
    cancelled.dispose();
  }
  const cancellationWasmBytes = engine.memory.buffer.byteLength;
  context.diagnostic(JSON.stringify({ cancellation_wasm_bytes: cancellationWasmBytes, cancellations: 432 }));
});

function percentile(values, quantile) {
  const ordered = values.toSorted((left, right) => left - right);
  return ordered[Math.min(ordered.length - 1, Math.floor(ordered.length * quantile))];
}

function round(value) {
  return Math.round(value * 1_000) / 1_000;
}
