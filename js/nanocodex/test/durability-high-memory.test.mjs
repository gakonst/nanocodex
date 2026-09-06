import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { gunzipSync } from "node:zlib";
import { Agent, Subagents, Transport } from "../host/index.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";

class WaitingSocket extends EventTarget {
  readyState = 1;
  constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
  send() {}
  close() { this.readyState = 3; }
}

test("durable subagent messaging survives a WASM heap beyond the Worker subarray ceiling", async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const engine = await initializeBrowserEngine({ module });
  // Reserve address space without filling it. This puts subsequent allocations
  // above 128 MiB without launching a delegation storm or contacting a model.
  const size = 144 * 1024 * 1024;
  const padding = engine.__wbindgen_export(size, 1);
  const nativeSubarray = Uint8Array.prototype.subarray;
  const store = createMemoryDurabilityStore("high-memory-messaging");
  const writes = [];
  const durability = { ...store, replace(stateId, request) {
    const result = store.replace(stateId, request);
    if (result.status === "replaced") writes.push({ stateId, payload: request.payload });
    return result;
  } };
  const options = { module, tools: [], durability, durabilityId: "high-memory-messaging",
    transport: Transport.openAi({ apiKey: "fixture", WebSocketImpl: WaitingSocket }) };
  let agent;
  try {
    // Cloudflare remote preview has this native V8 check. Node does not, so
    // retain the real WASM/SDK/store and emulate only the embedder constraint.
    Uint8Array.prototype.subarray = function(begin, end) {
      if (begin > 128 * 1024 * 1024) throw new RangeError("Invalid array buffer length");
      return nativeSubarray.call(this, begin, end);
    };
    assert.ok(engine.memory.buffer.byteLength > 128 * 1024 * 1024);
    agent = await Agent.create(options);
    const children = [];
    for (const role of ["one", "two"]) children.push(await Subagents.spawn(agent, {
      role, task: "Wait for directed checkpoint messages", outputSchema: { type: "object" },
    }));
    const initialWrites = writes.length;
    for (let index = 0; index < 8; index++) {
      const child = children[index % 2];
      const result = await Subagents.send(agent, {
        agentId: child.agent_id, priority: "urgent", purpose: "question", message: `Checkpoint Ελληνικά 😀 ${index}`,
      });
      assert.equal(result.to_agent_id, child.agent_id);
    }
    assert.ok(writes.length >= initialWrites + 8, "each message must reach the durable store");
    const persisted = writes.slice(initialWrites).map(({ payload }) => payload.startsWith("nanocodex-durable-state-gzip-v1:")
      ? gunzipSync(Buffer.from(payload.slice("nanocodex-durable-state-gzip-v1:".length), "base64")).toString("utf8")
      : payload).join("\n");
    assert.ok(persisted.includes("Checkpoint Ελληνικά 😀 7"));
    assert.equal((await Subagents.list(agent)).agents.length, 2);
    await agent.session.shutdown();
    agent = await Agent.create(options);
    const replacement = await Subagents.spawn(agent, {
      role: "after-reopen", task: "Verify checkpointing after reopen", outputSchema: { type: "object" },
    });
    assert.equal((await Subagents.send(agent, {
      agentId: replacement.agent_id, priority: "urgent", message: "Still durable after reopen 😀",
    })).to_agent_id, replacement.agent_id);
  } finally {
    Uint8Array.prototype.subarray = nativeSubarray;
    try { await agent?.session.shutdown(); }
    finally { engine.__wbindgen_export5(padding, size, 1); }
  }
});
