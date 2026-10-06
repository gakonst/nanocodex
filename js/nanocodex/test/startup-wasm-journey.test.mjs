import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { performance } from "node:perf_hooks";
import test from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";

test("precompiled host startup reaches HTTP, reopens durably, and preserves authorization", { timeout: 30_000 }, async t => {
  let calls = 0;
  let receivedDelta;
  const firstDelta = new Promise(resolve => { receivedDelta = resolve; });
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks));
    calls += 1;
    requests.push({ model: body.model, stateless: body.store === false, authorized: request.headers.authorization === "Bearer synthetic-startup" });
    if (request.headers.authorization !== "Bearer synthetic-startup") {
      response.writeHead(401, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "synthetic unauthorized", type: "invalid_request_error" } }));
      return;
    }
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.write(`data: ${JSON.stringify({ type: "response.output_text.delta", delta: "STARTUP_OK" })}\n\n`);
    if (calls === 1) await firstDelta;
    response.end(`data: ${JSON.stringify({ type: "response.completed", response: {
      id: `synthetic-${calls}`, status: "completed", output: [{ type: "message", role: "assistant",
        content: [{ type: "output_text", text: "STARTUP_OK" }] }],
      usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 },
    } })}\n\n`);
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const apiBaseUrl = `http://127.0.0.1:${server.address().port}/v1`;
  const transport = apiKey => Transport.openAi({ apiKey, apiBaseUrl, stateless: true });
  const options = {
    model: "gpt-6.1-sol", thinking: "low", toolMode: "direct",
    transport: transport("synthetic-startup"),
    [Symbol.for("nanocodex.browser.internalRuntime")]: { subagentsEnabled: false },
  };
  const module = await WebAssembly.compile(await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)));
  const durability = createMemoryDurabilityStore("synthetic-startup");
  const owned = { ...options, module, durability, durabilityId: "synthetic-startup" };
  const start = performance.now();
  const agent = await Agent.create(owned);
  const createMs = performance.now() - start;
  t.after(() => agent.session.shutdown());
  let firstDeltaMs;
  const watch = agent.events.watch();
  watch.onEvent(event => {
    if (event.type === "assistant.delta") {
      firstDeltaMs ??= performance.now() - start;
      receivedDelta(event.payload.text);
    }
  });
  t.after(() => watch.off());
  const first = agent.turn.prompt({ id: "first", input: "Synthetic startup request." });
  const result = await first.result();
  assert.equal(result.finalMessage, "STARTUP_OK");
  assert.equal(await firstDelta, "STARTUP_OK", "a live answer arrives before provider completion");
  const firstResultMs = performance.now() - start;
  result.dispose(); first.dispose();
  await agent.session.shutdown();
  const reopenStart = performance.now();
  const reopened = await Agent.create(owned);
  const reopenMs = performance.now() - reopenStart;
  t.after(() => reopened.session.shutdown());
  const replay = reopened.turn.prompt({ id: "first", input: "Synthetic startup request." });
  const replayResult = await replay.result();
  assert.equal(replayResult.finalMessage, "STARTUP_OK");
  assert.equal(calls, 1, "durable receipt replay does not call the provider");
  replayResult.dispose(); replay.dispose();
  const warm = reopened.turn.prompt({ id: "second", input: "Synthetic warm request." });
  const warmResult = await warm.result();
  assert.equal(warmResult.finalMessage, "STARTUP_OK");
  warmResult.dispose(); warm.dispose();
  await reopened.session.shutdown();
  const denied = await Agent.create({ ...options, module, transport: transport("synthetic-denied") });
  t.after(() => denied.session.shutdown());
  const rejected = denied.turn.prompt({ input: "Synthetic denied request." });
  await assert.rejects(rejected.result(), /401|unauthorized/i);
  rejected.dispose();
  await denied.session.shutdown();
  assert.equal(calls, 3, "authorization rejection is terminal and not retried");
  assert.deepEqual(requests.map(request => request.authorized), [true, true, false]);
  assert.ok(requests.every(request => request.model === "gpt-6.1-sol" && request.stateless));
  const evidence = { clock: "native node:perf_hooks; not Workers CPU timing", createMs, reopenMs, firstDeltaMs, firstResultMs,
    requests, durableReplayProviderCalls: 0 };
  await mkdir("output/sdk-startup", { recursive: true });
  await writeFile("output/sdk-startup/public-journey.json", JSON.stringify(evidence, null, 2));
  t.diagnostic(JSON.stringify(evidence));
});

test("failed module initialization releases its session and permits a compiled retry", async () => {
  // A fresh process exercises the uninitialized public SDK independently of the
  // successful startup above, without resetting private engine state.
  const { execFile } = await import("node:child_process");
  const { promisify } = await import("node:util");
  const script = `
    import assert from "node:assert/strict";
    import { readFile } from "node:fs/promises";
    import { Agent, Transport } from ${JSON.stringify(new URL("../host/index.mjs", import.meta.url).href)};
    const options = { sessionId: "018e65f5-0000-7000-8000-000000000101",
      transport: Transport.openAi({ apiKey: "synthetic-retry", stateless: true }) };
    await assert.rejects(Agent.create({ ...options, module: new Uint8Array([0,1,2,3]) }), WebAssembly.CompileError);
    const module = await WebAssembly.compile(await readFile(new URL(${JSON.stringify(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url).href)})));
    const agent = await Agent.create({ ...options, module });
    assert.equal(agent.sessionId, options.sessionId);
    await agent.session.shutdown();
    console.log("INITIALIZATION_RETRY_OK");
  `;
  const { stdout } = await promisify(execFile)(process.execPath, ["--input-type=module", "-e", script], { timeout: 10_000 });
  assert.equal(stdout.trim(), "INITIALIZATION_RETRY_OK");
});
