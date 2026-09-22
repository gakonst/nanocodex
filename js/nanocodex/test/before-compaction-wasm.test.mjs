import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import WebSocket from "ws";
import * as BrowserAgent from "../browser/Agent.mjs";
import { Agent as NodeAgent, Transport as NodeTransport } from "../node/index.mjs";
import { Agent as HostAgent, Transport as HostTransport, Subagents } from "../host/index.mjs";
import { startResponsesServer, send, sendCompaction } from "./support/responses.mjs";

const runtimes = [
  { name: "Node", Agent: NodeAgent, Transport: NodeTransport },
  { name: "Web API host", Agent: HostAgent, Transport: HostTransport },
];

async function within(promise, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), 3_000);
    })]);
  } finally { clearTimeout(timer); }
}

async function fixture(t, runtime, options = {}, { totalTokens = 100, output } = {}) {
  const server = await startResponsesServer();
  t.after(() => server.close());
  const requests = [];
  server.websocketServer.on("connection", socket => {
    socket.on("message", encoded => {
      const body = JSON.parse(encoded.toString());
      const compact = body.input.some(item => item.type === "compaction_trigger");
      requests.push({ phase: compact ? "compact" : "generate", body });
      const id = `fixture-response-${requests.length}`;
      if (compact) {
        sendCompaction(socket, id);
      } else {
        const items = output?.(requests) ?? [{ type: "message", role: "assistant",
          content: [{ type: "output_text", text: "fixture finished" }] }];
        send(socket, { type: "response.completed", response: {
          id, status: "completed", end_turn: !items.some(item => item.type === "function_call"),
          output: items,
          usage: { input_tokens: totalTokens - 5, output_tokens: 5, total_tokens: totalTokens },
        } });
      }
    });
  });
  const agent = await runtime.Agent.create({
    ...(runtime.Agent === HostAgent ? {
      module: await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)),
      harness: false,
    } : {}),
    tools: [], mcp: false, rawApiEvents: false,
    transport: runtime.Transport.openAi({ apiKey: "fixture", websocketUrl: server.url,
      websocketWarmup: false, ...(runtime.Agent === HostAgent ? { WebSocketImpl: WebSocket } : {}) }),
    ...options,
  });
  t.after(() => agent.session.shutdown().catch(() => {}));
  return { agent, requests, phases: () => requests.map(request => request.phase) };
}

for (const runtime of runtimes) {
  test(`real ${runtime.name} WASM awaits preservation before manual compaction`, { timeout: 15_000 }, async t => {
    const entered = Promise.withResolvers();
    const commit = Promise.withResolvers();
    const { agent, phases } = await fixture(t, runtime, {
      instructions: "PRIVATE_HARNESS_INSTRUCTIONS",
      beforeCompaction: input => { entered.resolve(input); return commit.promise; },
    });
    await agent.turn.prompt({ input: "Preserve the synthetic project decision." }).result();
    await agent.session.appendDeveloperMessage("PRIVATE_DEVELOPER_CONTEXT");
    const compacted = agent.session.compact();
    void compacted.catch(() => {});
    try {
      const input = await within(entered.promise, "beforeCompaction");
      assert.equal(input.sessionId, agent.sessionId);
      assert.equal(input.rootSessionId, agent.sessionId);
      assert.ok(input.boundaryId.length > 0);
      assert.equal(input.truncated, false);
      assert.deepEqual(input.messages, [
        { role: "user", text: "Preserve the synthetic project decision." },
        { role: "assistant", text: "fixture finished" },
      ]);
      assert.ok(Object.isFrozen(input));
      assert.ok(Object.isFrozen(input.messages));
      assert.ok(input.messages.every(Object.isFrozen));
      assert.equal(input.signal.aborted, false);
      // Give the local model server time to observe any incorrectly concurrent call.
      await new Promise(resolve => setTimeout(resolve, 25));
      assert.deepEqual(phases(), ["generate"], "the model cannot compact before preservation acknowledges");
      commit.resolve({ receiptId: "durable-manual-receipt" });
      await within(compacted, "manual compaction completion");
      assert.deepEqual(phases(), ["generate", "compact"]);
    } finally { commit.resolve({ receiptId: "fixture-cleanup" }); }
  });

  test(`real ${runtime.name} WASM leaves the hook disabled when omitted`, { timeout: 15_000 }, async t => {
    const { agent, phases } = await fixture(t, runtime);
    await agent.turn.prompt({ input: "Synthetic task without preservation." }).result();
    await agent.session.compact();
    assert.deepEqual(phases(), ["generate", "compact"]);
  });
}

test("real WASM awaits preservation before automatic compaction and the next generation", { timeout: 15_000 }, async t => {
  const entered = Promise.withResolvers();
  const commit = Promise.withResolvers();
  const { agent, phases } = await fixture(t, runtimes[1], {
    beforeCompaction: input => { entered.resolve(input); return commit.promise; },
  }, { totalTokens: 265639 });
  await agent.turn.prompt({ input: "Fill the synthetic context budget." }).result();
  const next = agent.turn.prompt({ input: "Continue after preservation." }).result();
  void next.catch(() => {});
  try {
    await within(entered.promise, "automatic preservation");
    await new Promise(resolve => setTimeout(resolve, 25));
    assert.deepEqual(phases(), ["generate"]);
    commit.resolve({ receiptId: "durable-automatic-receipt" });
    assert.equal((await within(next, "next generation")).finalMessage, "fixture finished");
    assert.deepEqual(phases(), ["generate", "compact", "generate"]);
  } finally { commit.resolve({ receiptId: "fixture-cleanup" }); }
});

for (const [name, callback, error] of [
  ["rejected preservation", async () => { throw new Error("synthetic durable write failed"); }, /synthetic durable write failed/],
  ["an invalid receipt", async () => ({ receiptId: "🦊".repeat(65) }), /receiptId/],
]) {
  test(`real WASM ${name} prevents compaction and retains history`, { timeout: 15_000 }, async t => {
    const { agent, phases } = await fixture(t, runtimes[1], { beforeCompaction: callback });
    await agent.turn.prompt({ input: "The original synthetic decision must survive." }).result();
    const before = await agent.session.context();
    await assert.rejects(agent.session.compact(), error);
    assert.deepEqual(phases(), ["generate"]);
    assert.deepEqual(await agent.session.context(), before);
  });
}

test("real WASM turn cancellation aborts a pending preservation without compacting", { timeout: 15_000 }, async t => {
  const entered = Promise.withResolvers();
  const commit = Promise.withResolvers();
  const { agent, phases } = await fixture(t, runtimes[1], {
    beforeCompaction: input => { entered.resolve(input); return commit.promise; },
  }, { totalTokens: 265639 });
  await agent.turn.prompt({ input: "Fill the synthetic context budget." }).result();
  const turn = agent.turn.prompt({ input: "Cancel while preservation is pending." });
  const result = turn.result();
  void result.catch(() => {});
  try {
    const input = await within(entered.promise, "cancellable preservation");
    await within(turn.cancel(), "turn cancellation");
    await assert.rejects(within(result, "cancelled turn result"), /cancel/i);
    assert.equal(input.signal.aborted, true);
    assert.deepEqual(phases(), ["generate"]);
    commit.resolve({ receiptId: "too-late-receipt" });
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(phases(), ["generate"], "late host completion cannot restart compaction");
  } finally { commit.resolve({ receiptId: "fixture-cleanup" }); }
});

test("real WASM subagents do not inherit the root preservation hook", { timeout: 15_000 }, async t => {
  const preserved = [];
  const { agent, phases } = await fixture(t, runtimes[1], {
    tools: [...Subagents.create()],
    beforeCompaction: async input => { preserved.push(input); return { receiptId: "root-only-receipt" }; },
  }, {
    totalTokens: 265639,
    output: requests => requests.length === 1 ? [{ type: "function_call",
      call_id: "fixture-submit", name: "submit_result", arguments: JSON.stringify({ output: "child result" }),
    }] : undefined,
  });
  const child = await Subagents.spawn(agent, { role: "synthetic-child", task: "Return the synthetic result.", outputSchema: true });
  const waited = await within(Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 5_000 }), "child completion");
  assert.equal(waited.agents[0].status.state, "completed");
  assert.deepEqual(phases(), ["generate", "compact", "generate"]);
  assert.equal(preserved.length, 0, "child compaction cannot invoke a root host callback");
  await agent.turn.prompt({ input: "Root synthetic task." }).result();
  await agent.session.compact();
  assert.equal(preserved.length, 1);
  assert.equal(preserved[0].sessionId, agent.sessionId);
  assert.equal(preserved[0].rootSessionId, agent.sessionId);
});

test("browser Worker creation rejects beforeCompaction callbacks before starting a Worker", async () => {
  let called = false;
  await assert.rejects(BrowserAgent.create({ harness: false, beforeCompaction: async () => {
    called = true;
    return { receiptId: "unreachable" };
  } }), /beforeCompaction.*functions across the Worker boundary/);
  assert.equal(called, false);
});
