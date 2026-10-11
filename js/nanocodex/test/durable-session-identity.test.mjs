import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { codeEvaluator } from "./quickjs-fixture.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";

class WaitingSocket extends EventTarget {
  readyState = 1;
  constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
  send() {}
  close() { this.readyState = 3; }
}

test("a durable host Agent without a session ID is identified by its UUIDv7 state", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const stateId = "01a12244-cea3-782a-9609-7a921dadb8d8";
  const store = createMemoryDurabilityStore(stateId);
  const options = { module, codeEvaluator, tools: [], durability: store, durabilityId: stateId,
    transport: Transport.openAi({ apiKey: "fixture", WebSocketImpl: WaitingSocket }) };
  for (let open = 0; open < 2; open++) {
    const agent = await Agent.create(options);
    try {
      assert.equal(agent.sessionId, stateId);
      assert.equal(agent.session.info().sessionId, stateId);
    } finally { await agent.session.shutdown(); }
  }
  // A host-named identity still wins, exactly as a Durable Object persisted
  // before unified identities names its stored runtime session.
  const named = "01a12244-d5ac-7b43-94dc-b0dc8559e02a";
  const agent = await Agent.create({ ...options, sessionId: named });
  try { assert.equal(agent.sessionId, named); } finally { await agent.session.shutdown(); }
  // Other state IDs keep receiving fresh runtime identities.
  const other = createMemoryDurabilityStore("managed-agent-id");
  const fresh = await Agent.create({ ...options, durability: other, durabilityId: "managed-agent-id" });
  try { assert.match(fresh.sessionId, /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-/); } finally { await fresh.session.shutdown(); }
});

