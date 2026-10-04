import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";
import { asyncCompletionInputKey, deliverCompletion, resumeCompletion } from "../cloudflare/Agent.mjs";

test("native completion receipt fingerprints, typed delivery and cancel-on-admission", { timeout: 30000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const durabilityId = "async-completion-receipt-fixture";
  const durability = createMemoryDurabilityStore(durabilityId);
  const pending = [];
  class ModelSocket extends EventTarget {
    readyState = 1;
    constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
    close() { this.readyState = 3; }
    send(encoded) { pending.push({ socket: this, request: JSON.parse(encoded) }); }
    respond(index, endTurn) {
      this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ type: "response.completed", response: {
        id: `completion-response-${index}`, status: "completed", end_turn: endTurn,
        output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: endTurn ? "finished" : "continue" }] }],
        usage: { input_tokens: 20, output_tokens: 1, total_tokens: 21 },
      } }) }));
    }
  }
  const agent = await Agent.create({ module, tools: [], rawApiEvents: false, durability, durabilityId,
    transport: Transport.openAi({ apiKey: "fixture", WebSocketImpl: ModelSocket, websocketWarmup: false }) });
  const wait = async count => {
    for (let i = 0; pending.length < count && i < 1000; i++) await new Promise(resolve => setTimeout(resolve, 5));
    assert.equal(pending.length, count);
  };
  const operations = () => JSON.parse(durability.snapshot().payload).nanocodex_durable_state.operations;
  const receipt = { delivery_id: "typed-result", job_id: "job-typed", original_call_id: "original-exec",
    output: [{ text: "UNTRUSTED_TOOL_CONTENT", type: "input_text" }, { detail: "low", file_id: "file_fixture", type: "input_image" }] };
  try {
    const turn = agent.turn.prompt({ id: "original", input: "Observe the authorized background work." });
    const result = turn.result();
    await wait(1);
    await deliverCompletion(turn, receipt);
    const key = await asyncCompletionInputKey(receipt);
    assert.equal(operations().original.steer_receipts["async:typed-result"].input_key, key);
    const revision = durability.snapshot().revision;
    await deliverCompletion(turn, receipt);
    assert.equal(durability.snapshot().revision, revision);
    await assert.rejects(deliverCompletion(turn, { ...receipt, output: "changed" }), /different input/);
    pending[0].socket.respond(1, false);
    await wait(2);
    const items = pending[1].request.input;
    const pair = items.filter(item => item.call_id === "async_typed-result");
    assert.equal(pair.length, 2);
    assert.equal(pair[1].type, "custom_tool_call_output");
    assert.ok(pair[1].output.some(item => item.type === "input_image" && item.file_id === "file_fixture"));
    assert.equal(items.filter(item => item.role === "user" && JSON.stringify(item).includes("UNTRUSTED_TOOL_CONTENT")).length, 0);
    pending[1].socket.respond(2, true);
    assert.equal((await result).finalMessage, "finished");
    const cancelled = resumeCompletion(agent, { ...receipt, delivery_id: "cancelled-result" }, { cancelOnAdmission: true });
    await assert.rejects(cancelled.result(), /cancel/i);
    assert.equal(pending.length, 2, "cancel-on-admission dispatches no inference");
    const idleReceipt = { ...receipt, delivery_id: "idle-result", output: "idle completion" };
    const idle = resumeCompletion(agent, idleReceipt);
    const idleResult = idle.result();
    await wait(3);
    pending[2].socket.respond(3, true);
    assert.equal((await idleResult).finalMessage, "finished");
    assert.equal((await resumeCompletion(agent, idleReceipt).result()).finalMessage, "finished");
    assert.equal(pending.length, 3, "terminal receipt replay makes no provider request");
    await mkdir("output", { recursive: true });
    await writeFile("output/async-completion-receipt-journey.json", JSON.stringify({
      command: "node --test js/nanocodex/test/async-completion-receipt-wasm.test.mjs",
      native_receipt_key: key, delivered_types: pair.map(item => item.type),
      content_types: pair[1].output.map(item => item.type), provider_requests: pending.length,
      cancellation_inference_requests: 0, replay_inference_requests: 0,
    }, null, 2));
  } finally { await agent.session.shutdown().catch(() => {}); }
});
