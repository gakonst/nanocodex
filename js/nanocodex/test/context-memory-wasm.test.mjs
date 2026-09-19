import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { toWasmConfig } from "../internal.mjs";

function modelFixture(totalTokens = 110) {
  const requests = [];
  class ModelSocket extends EventTarget {
    readyState = 1;
    constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event("open"))); }
    close() { this.readyState = 3; }
    send(encoded) {
      const request = JSON.parse(encoded);
      const compact = request.input.some(item => item.type === "compaction_trigger");
      requests.push({ phase: compact ? "compact" : "generate", bytes: Buffer.byteLength(encoded),
        previousResponseId: request.previous_response_id });
      const index = requests.length;
      const message = body => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(body) }));
      queueMicrotask(() => {
        if (compact) message({ type: "response.output_item.done", item: {
          id: `compact-${index}`, type: "compaction", encrypted_content: "synthetic-summary",
        } });
        message({ type: "response.completed", response: {
          id: `response-${index}`, status: "completed", end_turn: true,
          output: compact ? [] : [{ type: "message", role: "assistant",
            content: [{ type: "output_text", text: "finished" }] }],
          usage: { input_tokens: compact ? 105 : totalTokens - 5, output_tokens: 5, total_tokens: compact ? 110 : totalTokens },
        } });
      });
    }
  }
  return { requests, transport: Transport.openAi({ apiKey: "fixture", websocketWarmup: false,
    createWebSocket: () => ({ socket: new ModelSocket(), reasoningIncluded: true }) }) };
}

test("the raw event option preserves the SDK default", () => {
  assert.equal(toWasmConfig({ apiKey: "fixture" }).raw_api_events, undefined);
  assert.equal(toWasmConfig({ apiKey: "fixture", rawApiEvents: false }).raw_api_events, false);
});

for (const rawApiEvents of [undefined, false]) {
  test(`real WASM large generation and full-history compaction, rawApiEvents=${rawApiEvents}`, { timeout: 60_000 }, async t => {
    const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
    const fixture = modelFixture();
    const options = { module, tools: [], harness: false, rawApiEvents,
      instructions: "synthetic large prefix " + "x".repeat(8 * 1024 * 1024), transport: fixture.transport };
    let agent = await Agent.create(options);
    const bridge = globalThis.nanocodexHost;
    let rawCrossings = 0;
    let rawBytes = 0;
    const kinds = new Set();
    const watch = current => current.events.watch().onEvent(event => kinds.add(event.type));
    // Observe actual WASM string crossings before the SDK parses event envelopes.
    const instrumentBridge = () => { globalThis.nanocodexHost = { ...bridge, emitEvent(sessionId, encoded, ...args) {
      if (encoded.includes('"type":"api.event"')) {
        rawCrossings++;
        rawBytes += Buffer.byteLength(encoded);
      }
      return bridge.emitEvent(sessionId, encoded, ...args);
    } }; };
    instrumentBridge();
    try {
      watch(agent);
      const result = await agent.turn.prompt({ input: "synthetic task" }).result();
      assert.equal(result.finalMessage, "finished");
      const resume = await result.snapshot();
      await agent.session.shutdown();
      agent = await Agent.create({ ...options, resume });
      instrumentBridge();
      watch(agent);
      await agent.session.compact();
      assert.deepEqual(fixture.requests.map(r => r.phase), ["generate", "compact"]);
      assert.ok(fixture.requests.every(r => r.bytes > 8 * 1024 * 1024));
      assert.ok(fixture.requests.every(r => r.previousResponseId === undefined), "reopened compaction must replay full history");
      assert.ok(kinds.has("model.connection.completed"));
      assert.ok(kinds.has("model.call.completed"));
      assert.ok(kinds.has("model.compaction.completed"));
      if (rawApiEvents === false) {
        assert.equal(rawCrossings, 0);
        assert.equal(rawBytes, 0);
        assert.equal(kinds.has("api.event"), false);
      } else {
        assert.ok(rawCrossings >= 4);
        assert.ok(rawBytes > 16 * 1024 * 1024);
        assert.ok(kinds.has("api.event"));
      }
      t.diagnostic(JSON.stringify({ requests: fixture.requests, rawCrossings, rawBytes }));
    } finally {
      await agent.session.shutdown().catch(() => {});
      globalThis.nanocodexHost = bridge;
    }
  });
}

test("real WASM completed snapshot preserves the next-turn compaction decision", { timeout: 60_000 }, async () => {
  const fixture = modelFixture(265639);
  const options = { tools: [], harness: false, rawApiEvents: false, transport: fixture.transport };
  let agent = await Agent.create(options);
  try {
    const first = await agent.turn.prompt({ input: "synthetic task" }).result();
    const resume = JSON.parse(JSON.stringify(await first.snapshot()));
    assert.equal(resume.context_usage.usage.total_tokens, 265639);
    assert.equal(resume.context_usage.server_reasoning_included, true);
    await agent.turn.prompt({ input: "continue live" }).result();
    assert.deepEqual(fixture.requests.map(r => r.phase), ["generate", "compact", "generate"]);
    await agent.session.shutdown();
    agent = await Agent.create({ ...options, resume });
    await agent.turn.prompt({ input: "continue reopened" }).result();
    assert.deepEqual(fixture.requests.slice(3).map(r => r.phase), ["compact", "generate"]);
    assert.equal(fixture.requests[3].previousResponseId, undefined);
  } finally {
    await agent.session.shutdown().catch(() => {});
  }
});
