// Actual generated Rust WASM only. Missing build outputs are a failure, not a skip.
// Every Messages request goes to loopback, with synthetic credentials; no subscriptions.
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { createSqliteDurabilityStore, sqliteDurabilitySchema } from "../runtime/durability-store.mjs";

const signed = [
  { type: "thinking", thinking: "synthetic authorized effect", signature: "opaque-signature/+==", binding: { raw: "retain exact payload" } },
  { type: "redacted_thinking", data: "opaque-redacted/+==", binding: "unchanged" },
  { type: "server_tool_use", id: "server-search", name: "web_search", input: { query: "synthetic" }, opaque: { ordinal: 1 } },
  { type: "web_search_tool_result", tool_use_id: "server-search", content: [{ type: "web_search_result", url: "https://example.invalid/fixture", title: "fixture", encrypted_content: "opaque-encrypted" }], opaque: "native-result" },
  { type: "mcp_tool_use", id: "mcp-read", name: "read", server_name: "fixture", input: {}, native_extension: "mcp-call" },
  { type: "mcp_tool_result", tool_use_id: "mcp-read", content: [{ type: "text", text: "native MCP receipt", custom: { retained: true } }], native_extension: "mcp-result" },
  { type: "tool_use", id: "effect-once", name: "effect", input: { key: "a" }, caller: { type: "direct" } },
];
const receipt = [
  { type: "text", text: "effect committed", opaque: "receipt-extension" },
  { type: "image", source: { type: "base64", media_type: "image/png", data: "cG5n" } },
];
const text = value => [{ type: "text", text: value }];

function sse(blocks, stop = "end_turn", inputTokens = 12) {
  const frames = [{ type: "message_start", message: { id: "synthetic-response", role: "assistant", model: "fixture-model", content: [], usage: { input_tokens: inputTokens, output_tokens: 0, cache_read_input_tokens: 7, cache_creation_input_tokens: 3 }, container: { id: "stable-container" } } }];
  blocks.forEach((block, index) => {
    frames.push({ type: "content_block_start", index, content_block: block });
    frames.push({ type: "content_block_stop", index });
  });
  frames.push({ type: "message_delta", delta: { stop_reason: stop }, usage: { output_tokens: 5 } }, { type: "message_stop" });
  return frames.map(frame => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join("");
}

async function fixture(t, respond) {
  const requests = [];
  const sockets = new Set();
  const server = createServer(async (request, response) => {
    try {
      assert.equal(request.url, "/v1/messages");
      assert.equal(request.method, "POST");
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const wire = Buffer.concat(chunks).toString();
      const body = JSON.parse(wire);
      requests.push({ body, wire, headers: request.headers });
      response.writeHead(200, { "content-type": "text/event-stream", "access-control-allow-origin": "*" });
      const reply = await respond(requests.length, body, response);
      if (reply !== undefined) response.end(reply);
    } catch (error) {
      response.destroy(error);
    }
  });
  server.on("connection", socket => { sockets.add(socket); socket.once("close", () => sockets.delete(socket)); });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  t.after(async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => server.close(resolve)); });
  return { endpoint: `http://127.0.0.1:${server.address().port}/v1/messages`, requests };
}

function sqlite(path) {
  const db = new DatabaseSync(path);
  sqliteDurabilitySchema.forEach(sql => db.exec(sql));
  const store = createSqliteDurabilityStore({ transaction(callback) {
    db.exec("BEGIN IMMEDIATE");
    try {
      const value = callback((sql, params = []) => {
        const statement = db.prepare(sql);
        return /^\s*(SELECT|WITH)\b/i.test(sql) ? statement.all(...params).map(row => ({ ...row })) : (statement.run(...params), []);
      });
      assert.equal(typeof value?.then, "undefined", "native SQLite transactions must remain synchronous");
      db.exec("COMMIT");
      return value;
    } catch (error) { db.exec("ROLLBACK"); throw error; }
  } });
  return { store, close: () => db.close(), stateText: () => JSON.stringify([
    ...db.prepare("SELECT payload FROM nanocodex_durable_states").all(),
    ...db.prepare("SELECT value FROM nanocodex_durable_records").all(),
  ]) };
}

async function sdk(target = "node") {
  const namespace = await import(target === "node" ? "../node/index.mjs" : "../browser/index.mjs");
  assert.equal(typeof namespace.Claude?.create, "function", `${target} SDK must export opt-in Claude.create`);
  return namespace.Claude;
}
const input = (message, requestId) => ({ input: message, id: requestId });
const run = (agent, message, requestId) => agent.turn.prompt(input(message, requestId)).result();
const shutdown = agent => agent.session.shutdown();

// Additional acceptance scenarios below share this real HTTP/SSE fixture, not fake engines.

for (const [target, subscription] of [["node",false], ["browser",false], ["node",true], ["browser",true]]) {
  test(`actual ${target} ${subscription ? "OMP subscription" : "API-key"} WASM Messages tools/compaction survive SQLite reopen and terminal replay`, { timeout: 30_000 }, async t => {
    const Claude = await sdk(target);
    const directory = await mkdtemp(join(tmpdir(), "nanoclaude-wasm-"));
    t.after(() => rm(directory, { recursive: true, force: true }));
    const path = join(directory, "state.sqlite");
    const { endpoint, requests } = await fixture(t, index => {
      if (index === 1) return sse(subscription ? signed.map(block => block.type === "tool_use" ? {...block, name: "_" + block.name} : block) : signed, "tool_use", 70_000);
      if (index === 2) return sse(text("Preserve the synthetic original task."));
      if (index === 3) return 'event: error\ndata: {"type":"error","error":{"type":"invalid_request_error","message":"synthetic followup failure"}}\n\n';
      assert.equal(index, 4, "completed model receipts must not dispatch again");
      return sse(text("RECOVERED_NATIVE_WASM"));
    });
    let effects = 0;
    let authCalls = 0;
    const invocations = [];
    const options = {
      endpoint, model: "fixture-model", cache: "1h",
      ...(subscription ? {compatibilityProfile: "subscription", subscriptionIdentity: {installId: "synthetic-wasm-install", platform: "linux", arch: "x64"}} : {}),
      contextWindowTokens: 100_000, autoCompactWindowTokens: 100_000,
      ...(target === "browser" ? { module: await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)) } : {}),
      auth: { headers: async () => { authCalls++; return { "x-api-key": "synthetic-only" }; } },
      tools: [{ name: "effect", description: "Synthetic effect", inputSchema: { type: "object", properties: { key: { type: "string" } }, required: ["key"] }, handler: (value, invocation) => {
        effects++; invocations.push(invocation);
        assert.deepEqual(value, { key: "a" });
        return { content: receipt, isError: false, structuredResult: { committed: true } };
      } }],
      durabilityId: `native-${target}-${subscription}`,
    };
    let database = sqlite(path);
    let agent;
    try {
      agent = await Claude.create({ ...options, durability: database.store });
      await assert.rejects(run(agent, "perform one synthetic effect", "first"), /synthetic followup failure/);
      assert.equal(effects, 1);
      assert.equal(requests.length, 3, "automatic compaction executed a real SSE summary request");
      assert.deepEqual(requests[2].body.messages[1].content, subscription ? signed.map(block => block.type === "tool_use" ? {...block,name: "_" + block.name} : block) : signed);
      assert.deepEqual(requests[2].body.messages[2].content[0].content, receipt);
      assert.equal(requests[2].body.messages[2].content[0].tool_use_id, "effect-once");
      assert.deepEqual(requests[0].body.cache_control, { type: "ephemeral", ttl: "1h" });
      assert.deepEqual(requests[0].body.tools.map(tool => tool.name), [subscription ? "_effect" : "effect"]);
      assert.equal(requests[0].headers["x-api-key"], "synthetic-only");
      assert.equal(invocations[0].callId, "effect-once");
      assert.ok(invocations[0].sessionId && invocations[0].turnId);
      await shutdown(agent); agent = undefined;
      database.close(); database = sqlite(path);
      agent = await Claude.create({ ...options, durability: database.store });
      const result = await run(agent, "reconcile existing receipt", "recovery");
      assert.equal(result.finalMessage, "RECOVERED_NATIVE_WASM");
      const usage = await result.usage();
      assert.ok(usage);
      assert.equal(effects, 1, "completed tool receipt retained across actual disk reopen");
      assert.equal(requests[3].body.container, "stable-container");
      assert.deepEqual(requests[3].body.messages.slice(0, 3), requests[2].body.messages);
      assert.deepEqual(requests[3].body.tools, requests[0].body.tools);
      if (subscription) {
        const identities=requests.map(({body})=>JSON.parse(body.metadata.user_id));
        assert.equal(new Set(identities.map(id=>id.device_id)).size,1);
        assert.equal(new Set(identities.map(id=>id.session_id)).size,1);
        for (const {body,headers,wire} of requests) {
          assert.equal(headers['user-agent'],'claude-cli/2.1.280 (external, cli)');
          assert.equal(headers['x-stainless-runtime-version'],'v26.3.0');
          assert.equal(headers['accept'],'application/json');
          assert.match(body.system[0].text,/^x-anthropic-billing-header: cc_version=2\.1\.280\.[0-9a-f]{3}; cc_entrypoint=cli; cch=[0-9a-f]{5};$/);
          assert(wire.includes('"system":[{"type":"text","text":"x-anthropic-billing-header:'));
          assert.equal(body.system[1].text,"You are Claude Code, Anthropic's official CLI for Claude.");
        }
        assert(database.stateText().includes('subscription_wire_v1'), 'durable model identity contains final attested wire bytes');
      }
      await shutdown(agent); agent = undefined;
      database.close(); database = sqlite(path);
      const beforeAuth = authCalls;
      agent = await Claude.create({ ...options, durability: database.store,
        auth: { headers: () => { authCalls++; throw new Error("terminal replay must never authenticate"); } },
        tools: [{ ...options.tools[0], handler: () => { effects++; throw new Error("terminal replay must never dispatch tools"); } }],
      });
      const replay = await run(agent, "reconcile existing receipt", "recovery");
      assert.equal(replay.finalMessage, result.finalMessage);
      assert.deepEqual(await replay.usage(), usage);
      assert.equal(authCalls, beforeAuth, "reopened matching terminal replay uses zero auth callbacks");
      assert.equal(requests.length, 4, "reopened terminal replay uses zero Messages requests");
      assert.equal(effects, 1, "reopened terminal replay uses zero tool handlers");
      assert.doesNotMatch(database.stateText(), /synthetic-only|x-api-key|terminal replay must never authenticate/,
        "durable payloads and immutable records must not retain auth credentials or callbacks");
      await assert.rejects(run(agent, "different input", "recovery"), /different|conflict|identity|request/i);
      assert.equal(authCalls, beforeAuth);
      t.diagnostic(`actual ${target} ${subscription ? "OMP subscription" : "API-key"}: 4 loopback requests including compaction/failure/recovery; effect=1; terminal auth/network/tool deltas=0`);
    } finally { if (agent) await shutdown(agent).catch(() => {}); database.close(); }
  });
}

for (const [cache, expected] of [
  ["off", undefined], ["5m", { type: "ephemeral" }], ["1h", { type: "ephemeral", ttl: "1h" }],
]) {
  test(`actual WASM explicit cache policy ${cache}`, { timeout: 10_000 }, async t => {
    const Claude = await sdk();
    const { endpoint, requests } = await fixture(t, () => sse(text("CACHE_OK")));
    const agent = await Claude.create({ endpoint, model: "fixture-model", auth: { apiKey: "synthetic-only" }, cache });
    try {
      assert.equal((await run(agent, "cache fixture")).finalMessage, "CACHE_OK");
      assert.deepEqual(requests[0].body.cache_control, expected);
      assert.equal(requests[0].body.tools?.length ?? 0, 0, "no ambient tools, exec or tool_search are installed");
      assert.equal(requests[0].body.model, "fixture-model");
    } finally { await shutdown(agent); }
  });
}

test("actual WASM host-auth failure is redacted and recoverable without ambient credentials", { timeout: 10_000 }, async t => {
  const Claude = await sdk();
  const { endpoint, requests } = await fixture(t, () => sse(text("AUTH_RECOVERED")));
  let unavailable = true;
  let calls = 0;
  const agent = await Claude.create({ endpoint, model: "fixture-model", auth: { headers: () => {
    calls++;
    if (unavailable) throw new Error("SYNTHETIC_SECRET_MUST_NOT_LEAK");
    return { "x-api-key": "synthetic-recovered" };
  } } });
  try {
    const error = await run(agent, "auth unavailable").then(() => { throw new Error("expected auth failure"); }, error => error);
    assert.match(String(error), /auth|unavailable/i);
    assert.doesNotMatch(String(error), /SYNTHETIC_SECRET_MUST_NOT_LEAK/);
    assert.equal(requests.length, 0);
    unavailable = false;
    assert.equal((await run(agent, "auth recovered")).finalMessage, "AUTH_RECOVERED");
    assert.equal(requests.length, 1);
    assert.equal(calls, 2);
  } finally { await shutdown(agent); }
});

test("actual WASM host-tool throw becomes one redacted native error result", { timeout: 10_000 }, async t => {
  const Claude = await sdk();
  const { endpoint, requests } = await fixture(t, index => index === 1
    ? sse([{ type: "tool_use", id: "failed-effect", name: "effect", input: {} }], "tool_use")
    : sse(text("TOOL_FAILURE_HANDLED")));
  let calls = 0;
  const agent = await Claude.create({ endpoint, model: "fixture-model", auth: { apiKey: "synthetic-only" },
    tools: [{ name: "effect", description: "Synthetic throwing tool", handler: () => { calls++; throw new Error("SYNTHETIC_SECRET_MUST_NOT_LEAK"); } }],
  });
  try {
    assert.equal((await run(agent, "execute throwing fixture")).finalMessage, "TOOL_FAILURE_HANDLED");
    assert.equal(calls, 1);
    const block = requests[1].body.messages.at(-1).content[0];
    assert.equal(block.type, "tool_result");
    assert.equal(block.tool_use_id, "failed-effect");
    assert.equal(block.is_error, true);
    assert.doesNotMatch(JSON.stringify(requests[1].body), /SYNTHETIC_SECRET_MUST_NOT_LEAK/);
  } finally { await shutdown(agent); }
});

test("actual WASM cancellation aborts a live Messages fetch and allows the next prompt", { timeout: 15_000 }, async t => {
  const Claude = await sdk();
  const started = Promise.withResolvers();
  const aborted = Promise.withResolvers();
  const { endpoint, requests } = await fixture(t, (index, _body, response) => {
    if (index === 1) {
      response.write('event: message_start\ndata: {"type":"message_start","message":{"id":"blocked","role":"assistant","model":"fixture-model","content":[],"usage":{"input_tokens":0,"output_tokens":0}}}\n\n');
      response.once("close", () => aborted.resolve());
      started.resolve();
      return undefined;
    }
    return sse(text("CANCEL_RECOVERED"));
  });
  const agent = await Claude.create({ endpoint, model: "fixture-model", auth: { apiKey: "synthetic-only" } });
  try {
    const turn = agent.turn.prompt(input("wait for response"));
    const result = turn.result(); void result.catch(() => {});
    await started.promise;
    await turn.cancel();
    await assert.rejects(result, /cancel/i);
    await aborted.promise;
    assert.equal((await run(agent, "continue after cancellation")).finalMessage, "CANCEL_RECOVERED");
    assert.equal(requests.length, 2);
  } finally { await shutdown(agent); }
});

test("actual WASM live effect cancellation persists unknown outcome through SQLite reopen", { timeout: 15_000 }, async t => {
  const Claude = await sdk();
  const directory = await mkdtemp(join(tmpdir(), "nanoclaude-wasm-cancel-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "state.sqlite");
  const started = Promise.withResolvers();
  const { endpoint, requests } = await fixture(t, index => index === 1 ? sse(signed, "tool_use") : sse(text("UNKNOWN_RECONCILED")));
  let calls = 0;
  let invocation;
  const options = { endpoint, model: "fixture-model", auth: { apiKey: "synthetic-only" }, durabilityId: "effect-cancellation",
    tools: [{ name: "effect", description: "Synthetic interrupted effect", handler: (_value, context) => {
      calls++; invocation = context; started.resolve(); return new Promise(() => {});
    } }],
  };
  let database = sqlite(path);
  let agent;
  try {
    agent = await Claude.create({ ...options, durability: database.store });
    const turn = agent.turn.prompt(input("start a synthetic effect", "interrupted"));
    const result = turn.result(); void result.catch(() => {});
    await started.promise;
    await turn.cancel();
    await assert.rejects(result, /cancel/i);
    assert.equal(calls, 1);
    // Host signal abort is a separate contract from the Rust terminal outcome.
    assert.equal(invocation.signal.aborted, true, "cancellation must abort the host invocation signal");
    await shutdown(agent); agent = undefined;
    database.close(); database = sqlite(path);
    agent = await Claude.create({ ...options, durability: database.store,
      tools: [{ ...options.tools[0], handler: () => { calls++; return "must not run"; } }],
    });
    assert.equal((await run(agent, "reconcile interrupted effect", "reconcile")).finalMessage, "UNKNOWN_RECONCILED");
    assert.equal(calls, 1, "unknown effects are not redispatched");
    assert.equal(requests.length, 2);
    assert.deepEqual(requests[1].body.messages[1].content, signed);
    const unknown = requests[1].body.messages[2].content[0];
    assert.equal(unknown.tool_use_id, "effect-once");
    assert.equal(unknown.is_error, true);
    assert.match(unknown.content, /outcome unknown/i);
  } finally { if (agent) await shutdown(agent).catch(() => {}); database.close(); }
});

test("actual WASM cancelOnAdmission never issues a queued successor Messages request", { timeout: 15_000 }, async t => {
  const Claude = await sdk();
  const started = Promise.withResolvers();
  const { endpoint, requests } = await fixture(t, (index, _body, response) => {
    if (index === 1) { response.write(': synthetic blocked response\n\n'); started.resolve(); return undefined; }
    return sse(text("ADMISSION_CANCEL_RECOVERED"));
  });
  const agent = await Claude.create({ endpoint, model: "fixture-model", auth: { apiKey: "synthetic-only" } });
  try {
    const active = agent.turn.prompt({ input: "hold active request" });
    const activeResult = active.result(); void activeResult.catch(() => {});
    await started.promise;
    const queued = agent.turn.prompt({ input: "never dispatch successor", cancelOnAdmission: true });
    const queuedResult = queued.result(); void queuedResult.catch(() => {});
    await assert.rejects(queuedResult, /cancel/i);
    assert.equal(requests.length, 1, "cancelled queued prompt must never call Messages");
    await active.cancel();
    await assert.rejects(activeResult, /cancel/i);
    assert.equal((await run(agent, "recover after admission cancellation")).finalMessage, "ADMISSION_CANCEL_RECOVERED");
    assert.equal(requests.length, 2);
  } finally { await shutdown(agent); }
});

test("actual WASM Claude live route starts idle turns, steers active turns, and sends native media blocks", { timeout: 15_000 }, async t => {
  const Claude = await sdk();
  const started = Promise.withResolvers();
  const release = Promise.withResolvers();
  const { endpoint, requests } = await fixture(t, index => index === 1
    ? sse([{ type: "tool_use", id: "held", name: "hold", input: {} }], "tool_use")
    : sse(text(index === 2 ? "ROUTED_DONE" : "CONTENT_DONE")));
  const agent = await Claude.create({ endpoint, model: "fixture-model", auth: { apiKey: "synthetic-only" },
    tools: [{ name: "hold", description: "Held synthetic tool", handler: async () => { started.resolve(); await release.promise; return "held"; } }],
  });
  try {
    // Realtime voice frontends deliver live text through route: idle starts a turn.
    const turn = await agent.turn.route({ input: "book the flight" });
    assert.ok(turn, "idle live input starts a turn");
    const result = turn.result();
    await started.promise;
    // An active turn absorbs the next live input without a concurrent model call.
    assert.equal(await agent.turn.route({ input: "and ask for a window seat" }), undefined);
    release.resolve();
    assert.equal((await result).finalMessage, "ROUTED_DONE");
    assert.equal(requests.length, 2);
    assert.match(JSON.stringify(requests[1].body.messages), /and ask for a window seat/);

    const png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    const pdf = `data:application/pdf;base64,${Buffer.from("%PDF-1.4\n%%EOF\n").toString("base64")}`;
    const content = [{ type: "text", text: "describe both" }, { type: "image", image_url: png }, { type: "file", file_data: pdf, filename: "brief.pdf" }];
    assert.equal((await run(agent, content)).finalMessage, "CONTENT_DONE");
    const blocks = requests[2].body.messages.at(-1).content;
    assert.deepEqual(blocks.map(block => block.type), ["text", "image", "document"]);
    assert.deepEqual(blocks[1].source, { type: "base64", media_type: "image/png", data: png.split(",")[1] });
    assert.deepEqual(blocks[2].source, { type: "base64", media_type: "application/pdf", data: pdf.split(",")[1] });
    assert.equal(blocks[2].title, "brief.pdf");
    assert.throws(() => agent.turn.prompt({ input: [] }), /non-empty/);
    await assert.rejects(run(agent, [{ type: "file", file_data: "data:application/pdf;base64,bm90IGEgcGRm" }]), /document/i);
  } finally { await shutdown(agent); }
});
