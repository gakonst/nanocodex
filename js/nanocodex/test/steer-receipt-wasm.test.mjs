import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";
import { steerInputKey } from "../cloudflare/Agent.mjs";

test("real WASM atomic steering receipt recovers a lost storage ACK without reapplying input", { timeout: 60_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const durabilityId = "steer-ack-fixture";
  const store = createMemoryDurabilityStore(durabilityId);
  const pending = [];
  const input = [{ type: "text", text: "ONCE_STEER" }, { type: "image", image_url: "https://example.test/fixture.png", detail: "low" }];
  let lost = false;
  const operation = () => Object.values(JSON.parse(store.snapshot().payload).nanocodex_durable_state.operations)[0];
  const durability = { ...store, replace(id, request) {
    const current = Object.values(JSON.parse(request.payload).nanocodex_durable_state.operations)[0];
    const result = store.replace(id, request);
    if (!lost && current?.steer_receipts?.message) {
      lost = true;
      throw new Error("lost steering storage acknowledgement");
    }
    return result;
  } };
  class ModelSocket extends EventTarget {
    readyState = 1;
    constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
    close() { this.readyState = 3; }
    send(encoded) { pending.push({ socket: this, request: JSON.parse(encoded) }); }
    respond(endTurn, index) {
      this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({ type: "response.completed", response: {
        id: `response-${index}`, status: "completed", end_turn: endTurn,
        output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: endTurn ? "finished" : "continue" }] }],
        usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
      } }) }));
    }
  }
  const options = { module, harness: false, tools: [], rawApiEvents: false, durability, durabilityId,
    transport: Transport.openAi({ apiKey: "fixture", WebSocketImpl: ModelSocket, websocketWarmup: false }) };
  let agent = await Agent.create(options);
  const wait = async count => { for (let i = 0; pending.length < count && i < 500; i++) await new Promise(resolve => setTimeout(resolve, 5)); assert.equal(pending.length, count); };
  try {
    let turn = agent.turn.prompt({ input: "original task" });
    const interrupted = turn.result();
    void interrupted.catch(() => {});
    await wait(1);
    await assert.rejects(turn.steer({ input, messageId: "message" }), /lost steering storage acknowledgement/);
    await assert.rejects(interrupted);
    assert.equal(operation().steers.length, 1);
    assert.equal(operation().steer_receipts.message.input_key, await steerInputKey(input));
    await agent.session.shutdown().catch(() => {});
    agent = await Agent.create(options);
    turn = agent.turn.prompt({ input: "original task" });
    const finished = turn.result();
    await wait(2);
    const revision = store.snapshot().revision;
    await turn.steer({ input, messageId: "message" });
    assert.equal(store.snapshot().revision, revision, "replayed receipt performs no durable write");
    await assert.rejects(turn.steer({ input: "DIFFERENT", messageId: "message" }), /different input/);
    // The recovered pending identity remains withdrawable and cannot resurrect.
    assert.equal(await turn.withdrawSteer({ messageId: "message" }), true);
    await assert.rejects(turn.steer({ input, messageId: "message" }), /withdrawn/);
    await turn.steer({ input, messageId: "replacement" });
    await turn.steer({ input, messageId: "replacement" });
    pending[1].socket.respond(false, 1);
    await wait(3);
    assert.equal(JSON.stringify(pending[2].request).split("ONCE_STEER").length - 1, 1);
    const consumedRevision = store.snapshot().revision;
    await turn.steer({ input, messageId: "replacement" });
    assert.equal(store.snapshot().revision, consumedRevision, "a consumed receipt still replays without a second steer");
    pending[2].socket.respond(true, 2);
    assert.equal((await finished).finalMessage, "finished");
    assert.equal(operation().steer_receipts.message.withdrawn, true);
    assert.equal(operation().steer_receipts.replacement.withdrawn, false);
  } finally { await agent.session.shutdown().catch(() => {}); }
});
