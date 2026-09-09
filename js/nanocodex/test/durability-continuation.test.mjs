import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { gunzipSync } from "node:zlib";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";

function decode(payload) {
  const prefix = "nanocodex-durable-state-gzip-v1:";
  return JSON.parse(payload.startsWith(prefix)
    ? gunzipSync(Buffer.from(payload.slice(prefix.length), "base64")).toString()
    : payload).nanocodex_durable_state;
}

test("a long WASM turn resumes its current batch after a lost checkpoint acknowledgement", { timeout: 60_000 }, async (t) => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const engine = await initializeBrowserEngine({ module });
  let generations = 0;
  class ModelSocket extends EventTarget {
    readyState = 1;
    constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
    close() { this.readyState = 3; }
    send() {
      const index = ++generations;
      assert.ok(index <= 64, "recovery cannot resubmit settled model calls");
      queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
        type: "response.completed",
        response: { id: `response-${index}`, status: "completed", end_turn: index === 64,
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: index === 64 ? "finished" : `batch ${index}: ${"x".repeat(8192)}` }] }],
          usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 },
        },
      }) })));
    }
  }
  const durabilityId = "current-conversation";
  const store = createMemoryDurabilityStore(durabilityId);
  let failed = false;
  let maximumBytes = 0;
  const durability = { ...store, replace(id, request) {
    const state = decode(request.payload);
    const operation = Object.values(state.operations)[0];
    maximumBytes = Math.max(maximumBytes, JSON.stringify(operation).length);
    assert.ok(Object.keys(operation.steps).length <= 1, "historical effects must be retired");
    const result = store.replace(id, request);
    if (!failed && operation.retired_model_calls === 31) {
      failed = true;
      throw new Error("lost checkpoint acknowledgement");
    }
    return result;
  } };
  const options = { module, harness: false, tools: [], durability, durabilityId,
    transport: Transport.openAi({ apiKey: "fixture", WebSocketImpl: ModelSocket, websocketWarmup: false }) };
  let agent = await Agent.create(options);
  try {
    await assert.rejects(agent.turn.prompt({ input: "complete 64 batches" }).result(), /lost checkpoint acknowledgement/);
    assert.equal(failed, true);
    assert.equal(generations, 31);
    await agent.session.shutdown().catch(() => {});
    agent = await Agent.create(options);
    assert.equal((await agent.turn.prompt({ input: "complete 64 batches" }).result()).finalMessage, "finished");
    assert.equal(generations, 64);
    assert.ok(maximumBytes < 700_000, `retained ${maximumBytes} bytes for a 512 KiB conversation`);
    assert.ok(engine.memory.buffer.byteLength < 128 * 1024 * 1024, `WASM heap grew to ${engine.memory.buffer.byteLength}`);
    t.diagnostic(`maximum state: ${maximumBytes} bytes; WASM heap: ${engine.memory.buffer.byteLength} bytes`);
    const operation = Object.values(decode(store.snapshot().payload).operations)[0];
    assert.equal(operation.continuation, undefined);
    assert.deepEqual(operation.steps, {});
  } finally {
    await agent.session.shutdown().catch(() => {});
  }
});
