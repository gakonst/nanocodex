import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import { createMemoryDurabilityStore, exportDurabilityStatePage, importDurabilityStatePages } from "../runtime/durability-store.mjs";

function decode(payload) {
  return JSON.parse(payload).nanocodex_durable_state;
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
      assert.ok(index <= 65, "recovery cannot resubmit settled model calls");
      queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
        type: "response.completed",
        response: { id: `response-${index}`, status: "completed", end_turn: index >= 64,
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: index >= 64 ? "finished" : `batch ${index}: ${"x".repeat(8192)}` }] }],
          usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 },
        },
      }) })));
    }
  }
  const durabilityId = "current-conversation";
  const store = createMemoryDurabilityStore(durabilityId);
  let failed = false;
  let maximumBytes = 0;
  let recordBytesWritten = 0;
  let largestCommit = 0;
  const durability = { ...store, replace(id, request) {
    const bytes = request.records.reduce((total, record) => total + Buffer.byteLength(record.value), 0);
    recordBytesWritten += bytes;
    largestCommit = Math.max(largestCommit, bytes);
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
  const terminals = [];
  agent.events.watch().onEvent(event => { if (["run.failed", "run.completed"].includes(event.type)) terminals.push(event); });
  try {
    await assert.rejects(agent.turn.prompt({ input: "complete 64 batches" }).result(), /lost checkpoint acknowledgement/);
    assert.equal(failed, true);
    assert.equal(generations, 31);
    assert.deepEqual(terminals, [], "an interrupted durable attempt must not terminate its observer");
    await agent.session.shutdown().catch(() => {});
    agent = await Agent.create(options);
    assert.equal((await agent.turn.prompt({ input: "complete 64 batches" }).result()).finalMessage, "finished");
    assert.equal(generations, 64);
    assert.ok(maximumBytes < 8_192, `retained ${maximumBytes} bytes for a 512 KiB conversation`);
    assert.ok(engine.memory.buffer.byteLength < 128 * 1024 * 1024, `WASM heap grew to ${engine.memory.buffer.byteLength}`);
    assert.ok(largestCommit < 64_000, `one commit rewrote ${largestCommit} bytes of context`);
    t.diagnostic(`records written: ${recordBytesWritten} bytes; largest commit: ${largestCommit} bytes`);
    t.diagnostic(`maximum state: ${maximumBytes} bytes; WASM heap: ${engine.memory.buffer.byteLength} bytes`);
    const operation = Object.values(decode(store.snapshot().payload).operations)[0];
    assert.equal(operation.continuation, undefined);
    assert.deepEqual(operation.steps, {});
    await agent.session.shutdown();
    const destination = createMemoryDurabilityStore(durabilityId);
    let pages = 0;
    async function* archive() {
      let cursor;
      let to;
      do {
        const page = await exportDurabilityStatePage(store, durabilityId, { from: "0", to, cursor });
        assert.ok(page.records.length <= 16);
        pages++;
        yield page;
        to = page.to;
        cursor = page.nextCursor;
      } while (cursor !== null);
    }
    await importDurabilityStatePages(destination, archive());
    assert.ok(pages > 2, "portability must stream immutable records across multiple pages");
    let coldWrites = 0;
    agent = await Agent.create({ ...options, durability: { ...destination, replace(id, request) {
      coldWrites += request.records.reduce((n, record) => n + Buffer.byteLength(record.value), 0);
      return destination.replace(id, request);
    } } });
    assert.equal((await agent.turn.prompt({ input: "continue the imported conversation" }).result()).finalMessage, "finished");
    assert.equal(generations, 65);
    assert.ok(coldWrites < 64_000, `cold continuation rewrote ${coldWrites} bytes of old context`);
    t.diagnostic(`portable record pages: ${pages}; cold continuation writes: ${coldWrites} bytes`);
  } finally {
    await agent.session.shutdown().catch(() => {});
  }
});

for (const nested of [false, true]) {
  test(`a ${nested ? "nested" : "direct"} host interruption retains the unsettled effect`, { timeout: 60_000 }, async () => {
    let generations = 0;
    let dispatched = 0;
    const receipts = new Map();
    const observedIds = [];
    class ModelSocket extends EventTarget {
      readyState = 1;
      constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
      close() { this.readyState = 3; }
      send() {
        const index = ++generations;
        assert.ok(index <= 2, "settled model calls cannot be repeated");
        const call = nested
          ? { type: "custom_tool_call", call_id: "effect", name: "exec",
              input: "try { text(await tools.fixture({})); } catch (error) { text('guest caught it'); }" }
          : { type: "function_call", call_id: "effect", name: "exec_command", arguments: "{}" };
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          type: "response.completed", response: { id: `response-${index}`, status: "completed",
            output: index === 1 ? [call] : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "finished" }] }],
            usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 },
          },
        }) })));
      }
    }
    const durabilityId = `interrupted-${nested}`;
    const durability = createMemoryDurabilityStore(durabilityId);
    const options = { harness: false, durability, durabilityId,
      codeEvaluator: (source, { tools, text }) => {
        const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
        return new AsyncFunction("tools", "text", source)(tools, text);
      },
      tools: { [nested ? "fixture" : "exec_command"]: {
        description: "A durable fixture effect", parameters: { type: "object", properties: {} },
        handler(_input, context) {
          observedIds.push(context.callId);
          if (receipts.has(context.callId)) return receipts.get(context.callId);
          dispatched += 1;
          receipts.set(context.callId, "effect finished");
          throw Object.assign(new Error("lost host response"), { code: "host_interrupted" });
        },
      } },
      transport: Transport.openAi({ apiKey: "fixture", WebSocketImpl: ModelSocket, websocketWarmup: false }),
    };
    let agent = await Agent.create(options);
    try {
      await assert.rejects(agent.turn.prompt({ input: "run fixture" }).result(), /lost host response/);
      assert.equal(generations, 1);
      const pending = Object.values(decode(durability.snapshot().payload).operations)[0];
      assert.ok(pending.continuation);
      assert.ok(Object.values(pending.steps).some((step) => step.output === undefined));
      await agent.session.shutdown().catch(() => {});
      agent = await Agent.create(options);
      assert.equal((await agent.turn.prompt({ input: "run fixture" }).result()).finalMessage, "finished");
      assert.equal(generations, 2);
      assert.equal(dispatched, 1);
      assert.equal(observedIds.length, 2);
      assert.equal(observedIds[0], observedIds[1]);
    } finally {
      await agent.session.shutdown().catch(() => {});
    }
  });
}
