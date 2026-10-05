import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";

test("real WASM atomic steering receipt recovers a lost storage ACK without reapplying input", { timeout: 60_000 }, async (t) => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const durabilityId = "steer-ack-fixture";
  const store = createMemoryDurabilityStore(durabilityId);
  const pending = [];
  const input = [{ type: "text", text: "ONCE_STEER" }, { type: "image", image_url: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=", detail: "high" }];
  let lost = false;
  let loseNextRootAck = false;
  const durability = { ...store, replace(id, request) {
    const result = store.replace(id, request);
    // Arm this only while the original model request is waiting, immediately
    // before steering. Child documents and receipt storage stay opaque.
    if (id === durabilityId && loseNextRootAck && result.status === "replaced") {
      loseNextRootAck = false;
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
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000081",
    transport: Transport.openAi({ apiKey: "fixture", WebSocketImpl: ModelSocket, websocketWarmup: false }) };
  let agent = await Agent.create(options);
  const wait = async count => { for (let i = 0; pending.length < count && i < 500; i++) await new Promise(resolve => setTimeout(resolve, 5)); assert.equal(pending.length, count); };
  try {
    let turn = agent.turn.prompt({ input: "original task" });
    const interrupted = turn.result();
    void interrupted.catch(() => {});
    await wait(1);
    const beforeSteerRevision = store.snapshot().revision;
    loseNextRootAck = true;
    await assert.rejects(turn.steer({ input, messageId: "message" }), /lost steering storage acknowledgement/);
    await assert.rejects(interrupted);
    assert.equal(lost, true, "the store committed the steer before losing its acknowledgement");
    assert.equal(BigInt(store.snapshot().revision), BigInt(beforeSteerRevision) + 1n);
    await agent.session.shutdown().catch(() => {});
    agent = await Agent.create(options);
    turn = agent.turn.prompt({ input: "original task" });
    const finished = turn.result();
    void finished.catch(() => {});
    await wait(2);
    const revision = store.snapshot().revision;
    await turn.steer({ input, messageId: "message" });
    assert.equal(store.snapshot().revision, revision, "replayed receipt performs no durable write");
    await assert.rejects(turn.steer({ input: "DIFFERENT", messageId: "message" }), /different input/);
    await assert.rejects(turn.steer({ input: [input[0], { ...input[1], detail: "original" }], messageId: "message" }), /different input/);
    // The recovered pending identity remains withdrawable and cannot resurrect.
    assert.equal(await turn.withdrawSteer({ messageId: "message" }), true);
    await assert.rejects(turn.steer({ input, messageId: "message" }), /withdrawn/);
    await turn.steer({ input, messageId: "replacement" });
    await turn.steer({ input, messageId: "replacement" });
    pending[1].socket.respond(false, 1);
    await wait(3);
    assert.equal(JSON.stringify(pending[2].request).split("ONCE_STEER").length - 1, 1);
    assert.deepEqual(pending[2].request.input.flatMap((item) => item.content ?? []), [
      { type: "input_text", text: "ONCE_STEER" },
      { type: "input_image", image_url: input[1].image_url },
    ]);
    const consumedRevision = store.snapshot().revision;
    await turn.steer({ input, messageId: "replacement" });
    assert.equal(store.snapshot().revision, consumedRevision, "a consumed receipt still replays without a second steer");
    await assert.rejects(turn.steer({ input, messageId: "message" }), /withdrawn/);
    await assert.rejects(turn.steer({ input: "DIFFERENT", messageId: "replacement" }), /different input/);
    pending[2].socket.respond(true, 2);
    assert.equal((await finished).finalMessage, "finished");
    assert.equal(pending.length, 3, "only the original, recovered, and steered model requests ran");
    t.diagnostic(JSON.stringify({ lostAck: lost, beforeSteerRevision, recoveredRevision: revision,
      consumedRevision, modelRequests: pending.length, delivered: ["ONCE_STEER", "image:high"],
      duplicateWrites: 0, changedInputRejected: true, withdrawnIdentityRejected: true, finalMessage: "finished" }));
  } finally { await agent.session.shutdown().catch(() => {}); }
});
