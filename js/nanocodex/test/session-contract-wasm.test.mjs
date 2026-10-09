// Actual generated Rust WASM only: one harness-neutral session journey through
// the public Agent factory (Web API host flavor) for both harness families.
// Every model request goes to a loopback HTTP fixture with synthetic credentials.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { test } from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { codeEvaluator } from "./quickjs-fixture.mjs";

const module = await WebAssembly.compile(await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)));

async function loopback(t, path, reply) {
  const requests = [];
  const server = createServer(async (request, response) => {
    try {
      assert.equal(request.url, path);
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks).toString());
      requests.push(body);
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(reply(requests.length, body));
    } catch (error) { response.destroy(error); }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => { server.closeAllConnections(); return new Promise((resolve) => server.close(resolve)); });
  return { url: "http://127.0.0.1:" + server.address().port, requests };
}

const families = {
  async codex(t) {
    const fixture = await loopback(t, "/v1/responses", (index) => "data: " + JSON.stringify({
      type: "response.completed",
      response: { id: "resp-" + index, status: "completed", usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
        output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "REPLY_" + index }] }] },
    }) + "\n\n");
    return {
      fixture,
      options: { transport: Transport.openAi({ apiKey: "synthetic", apiBaseUrl: fixture.url + "/v1", stateless: true }),
        model: "gpt-6.1-sol", thinking: "low", codeEvaluator, tools: [], mcp: false, rawApiEvents: false, module },
      transcript: (body) => JSON.stringify(body.input),
      otherFamilyModel: "claude-sonnet-4-6",
    };
  },
  async claude(t) {
    const fixture = await loopback(t, "/v1/messages", (index) => [
      { type: "message_start", message: { id: "msg-" + index, role: "assistant", model: "fixture-model", content: [], usage: { input_tokens: 1, output_tokens: 0 } } },
      { type: "content_block_start", index: 0, content_block: { type: "text", text: "REPLY_" + index } },
      { type: "content_block_stop", index: 0 },
      { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 1 } },
      { type: "message_stop" },
    ].map((frame) => "event: " + frame.type + "\ndata: " + JSON.stringify(frame) + "\n\n").join(""));
    return {
      fixture,
      // Portable checkpoints pin a cataloged harness model; the endpoint is still loopback.
      options: { harness: "claude", endpoint: fixture.url + "/v1/messages", model: "claude-sonnet-4-6", maxTokens: 1024,
        codeEvaluator, auth: { apiKey: "synthetic-only" }, module },
      transcript: (body) => JSON.stringify(body.messages),
      otherFamilyModel: "gpt-6.1-sol",
    };
  },
};

for (const harness of ["codex", "claude"]) {
  test(harness + " sessions share one identity, checkpoint, fork, and resume contract", { timeout: 60_000 }, async (t) => {
    const { fixture, options, transcript, otherFamilyModel } = await families[harness](t);
    const agents = [];
    t.after(async () => { for (const agent of agents.reverse()) await agent.session.shutdown().catch(() => {}); });
    const root = await Agent.create(options);
    agents.push(root);

    const info = root.session.info();
    assert.equal(info.harness, harness);
    assert.equal(info.sessionId, root.sessionId);
    assert.deepEqual(info.lineage, { rootSessionId: root.sessionId, parentSessionId: null, origin: "root", depth: 0 });
    const capabilities = root.session.capabilities();
    assert.equal(capabilities.checkpoint, true);
    assert.equal(capabilities.fork, true);
    assert.equal(root.session.persistence(), null, "an in-memory session reports no persistence");
    await assert.rejects(root.session.setModel(otherFamilyModel), /another harness family/);
    // Service tier changes follow capabilities; an unselectable tier is an explicit capability gap.
    assert.notEqual(capabilities.serviceTier, "fixed");
    await root.session.setServiceTier("standard");
    await assert.rejects(root.session.setServiceTier("turbo"), /service tier must be/);
    await root.session.setServiceTier("ultrafast").then(
      () => root.session.setServiceTier("standard"),
      (error) => assert.equal(error.code, "unsupported_capability"),
    );

    const first = await root.turn.prompt({ input: "remember cobalt" }).result();
    assert.equal(first.finalMessage, "REPLY_1");
    // A stored checkpoint is plain JSON text that round-trips back into the API.
    const stored = JSON.parse(JSON.stringify(await first.checkpoint()));
    const latest = await root.session.checkpoint();
    assert.deepEqual(JSON.parse(JSON.stringify(latest)), latest, "session checkpoints are JSON-safe");

    const side = await root.session.fork({ at: stored, origin: "side_conversation" });
    agents.push(side);
    assert.equal(side.session.info().harness, harness);
    assert.deepEqual(side.session.info().lineage, {
      rootSessionId: root.sessionId, parentSessionId: root.sessionId, origin: "side_conversation", depth: 1,
    });
    assert.notEqual(side.sessionId, root.sessionId);
    assert.equal((await side.turn.prompt({ input: "what color?" }).result()).finalMessage, "REPLY_2");
    assert.match(transcript(fixture.requests[1]), /remember cobalt/);

    const branch = await root.session.fork({ at: first });
    agents.push(branch);
    assert.equal(branch.session.info().lineage.origin, "fork");
    await branch.turn.prompt({ input: "branch question" }).result();
    assert.match(transcript(fixture.requests[2]), /remember cobalt/);
    assert.doesNotMatch(transcript(fixture.requests[2]), /what color/, "forks do not share later history");

    const resumed = await Agent.create({ ...options, resume: stored });
    agents.push(resumed);
    assert.equal(resumed.session.info().harness, harness);
    await resumed.turn.prompt({ input: "resumed question" }).result();
    assert.match(transcript(fixture.requests.at(-1)), /remember cobalt/);
  });
}

test("checkpoints are family-tagged and never decoded by another harness", { timeout: 60_000 }, async (t) => {
  const codex = await families.codex(t);
  const claude = await families.claude(t);
  const codexAgent = await Agent.create(codex.options);
  const claudeAgent = await Agent.create(claude.options);
  t.after(async () => {
    await codexAgent.session.shutdown().catch(() => {});
    await claudeAgent.session.shutdown().catch(() => {});
  });
  const claudeCheckpoint = await (await claudeAgent.turn.prompt({ input: "claude turn" }).result()).checkpoint();
  const codexCheckpoint = await (await codexAgent.turn.prompt({ input: "codex turn" }).result()).checkpoint();
  await assert.rejects(Agent.create({ ...codex.options, resume: claudeCheckpoint }),
    (error) => error.code === "checkpoint_family_mismatch");
  await assert.rejects(Agent.create({ ...claude.options, resume: codexCheckpoint }),
    (error) => error.code === "checkpoint_family_mismatch");
  await assert.rejects(codexAgent.session.fork({ at: claudeCheckpoint }), (error) => error.code !== undefined);
  await assert.rejects(codexAgent.session.fork({ at: { format: "unknown" } }), (error) => error.code === "invalid_checkpoint");
});

