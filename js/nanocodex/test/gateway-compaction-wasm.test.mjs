import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";

const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
const summary = "The artifact build_cobalt passed 42 checks. Continue by reporting the result; do not run the build again.";
const completion = (content, finish_reason = "stop") => ({ choices: [{ finish_reason, message: { content } }],
  usage: { prompt_tokens: 140, completion_tokens: 30, total_tokens: 170 } });
function streamed(message, total = 50, finish_reason = "stop") {
  const frames = [
    { choices: [{ index: 0, delta: message, finish_reason: null }] },
    { choices: [{ index: 0, delta: {}, finish_reason }], usage: {
      prompt_tokens: total - 5, completion_tokens: 5, total_tokens: total,
    } },
  ];
  return new Response(frames.map(value => `data: ${JSON.stringify(value)}\n\n`).join("") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } });
}
async function fixture(t, { automatic = false, summaryResponse, provider = "openrouter" } = {}) {
  const requests = [];
  let executions = 0, generation = 0;
  const freshTransport = () => {
    const transport = createGatewayResponses({ provider, model: "gpt-6-astra", reasoningEffort: "low", apiKey: "synthetic-key",
      fetch: async (url, init) => {
        assert.equal(url, provider === "openrouter" ? "https://openrouter.ai/api/v1/chat/completions" : "https://ai-gateway.vercel.sh/v1/chat/completions");
        const body = JSON.parse(init.body);
        assert.equal(body.model, "openai/gpt-6-astra");
        requests.push(body);
        if (body.stream === false) {
          assert.equal(body.tools, undefined, "summarization cannot dispatch tools");
          assert.equal(body.tool_choice, undefined);
          return summaryResponse ? summaryResponse(body, init.signal) : Response.json(completion(summary));
        }
        generation++;
        if (generation === 1) {
          const tool = body.tools.find(tool => tool.function.description.startsWith("buildArtifact\n"));
          assert.ok(tool);
          return streamed({ tool_calls: [{ index: 0, id: "build-call", type: "function",
            function: { name: tool.function.name, arguments: "{}" } }] }, 40, "tool_calls");
        }
        return streamed({ content: generation === 2 ? "Build finished." : "42 checks passed; the build was not repeated." },
          automatic && generation === 2 ? 265639 : 50);
      } });
    return Transport.hostManaged({ ...transport, websocketPreconnect: false,
      createWebSocket() { assert.fail("gateway must not use WebSocket"); } });
  };
  const create = resume => Agent.create({ module, model: "gpt-6-astra", thinking: "low", toolMode: "direct",
    transport: freshTransport(), ...(resume ? { resume } : {}),
    tools: { buildArtifact: { description: "Build a synthetic artifact", parameters: { type: "object", additionalProperties: false },
      handler() { executions++; return { artifact: "build_cobalt", checks: 42, passed: true }; } } },
  });
  const agents = [];
  const open = async resume => { const agent = await create(resume); agents.push(agent); return agent; };
  t.after(async () => { for (const agent of agents) await agent.session.shutdown().catch(() => {}); });
  return { agent: await open(), open, requests, executions: () => executions };
}

for (const provider of ["openrouter", "vercel"]) test(`${provider}: compact, continue, restart and compact again retain tool results`, { timeout: 20000 }, async t => {
  const f = await fixture(t, { provider });
  await f.agent.turn.prompt({ input: "Build the artifact once and preserve the check count." }).result();
  await f.agent.session.compact();
  const request = f.requests.find(body => body.stream === false);
  assert.match(JSON.stringify(request.messages), /build_cobalt/);
  assert.match(JSON.stringify(request.messages), /42/);
  const result = await f.agent.turn.prompt({ input: "Report the check count without rerunning." }).result();
  assert.match(result.finalMessage, /42 checks passed/);
  assert.ok(f.requests.at(-1).messages.some(message => message.role === "assistant" && message.content?.includes(summary)));
  const snapshot = await result.snapshot();
  await f.agent.session.shutdown();
  const resumed = await f.open(JSON.parse(JSON.stringify(snapshot)));
  await resumed.session.compact();
  assert.ok(f.requests.at(-1).messages.some(message => message.content?.includes(summary)), "a second compaction sees the prior summary");
  assert.match((await resumed.turn.prompt({ input: "Continue reporting the preserved result." }).result()).finalMessage, /42/);
  assert.equal(f.executions(), 1);
  assert.equal(f.requests.filter(body => body.stream === false).length, 2);
  t.diagnostic(JSON.stringify({ provider, phases: f.requests.map(body => body.stream ? "generation" : "summary"), toolExecutions: f.executions(), restarted: true }));
});

test("OpenRouter automatically compacts at the context threshold and continues", { timeout: 20000 }, async t => {
  const f = await fixture(t, { automatic: true });
  await f.agent.turn.prompt({ input: "Build the artifact once." }).result();
  assert.match((await f.agent.turn.prompt({ input: "Continue after the context fills." }).result()).finalMessage, /42/);
  assert.equal(f.requests.filter(body => body.stream === false).length, 1);
  assert.equal(f.executions(), 1);
  t.diagnostic(JSON.stringify({ phases: f.requests.map(body => body.stream ? "generation" : "summary"), completed: true }));
});

for (const [name, value] of [
  ["empty", completion("  ")],
  ["truncated", completion("partial", "length")],
  ["refused", { choices: [{ finish_reason: "stop", message: { refusal: "synthetic refusal" } }] }],
]) test(`OpenRouter ${name} summary leaves the original history intact`, { timeout: 20000 }, async t => {
  const f = await fixture(t, { summaryResponse: () => Response.json(value) });
  await f.agent.turn.prompt({ input: "Build once; keep the result if compaction fails." }).result();
  const before = await f.agent.session.context();
  await assert.rejects(f.agent.session.compact(), /compaction|Gateway Responses/i);
  assert.deepEqual(await f.agent.session.context(), before);
  assert.equal(f.executions(), 1);
  t.diagnostic(JSON.stringify({ failure: name, historyPreserved: true }));
});


test("OpenRouter cancellation during automatic compaction preserves history and ignores a late summary", { timeout: 20000 }, async t => {
  const entered = Promise.withResolvers();
  const late = Promise.withResolvers();
  const f = await fixture(t, { automatic: true, summaryResponse: (_body, signal) => { entered.resolve(signal); return late.promise; } });
  await f.agent.turn.prompt({ input: "Build once before cancellation." }).result();
  const before = await f.agent.session.context();
  const turn = f.agent.turn.prompt({ input: "Continue after compaction." });
  const result = turn.result();
  void result.catch(() => {});
  try {
    const signal = await entered.promise;
    await turn.cancel();
    await assert.rejects(result, /cancel/i);
    assert.equal(signal.aborted, true);
    late.resolve(Response.json(completion(summary)));
    await new Promise(resolve => setImmediate(resolve));
    const after = await f.agent.session.context();
    for (const item of before.history) assert.ok(after.history.some(retained => JSON.stringify(retained) === JSON.stringify(item)));
    assert.equal(after.history.some(item => item.type === "compaction"), false);
    assert.equal(f.executions(), 1);
    assert.equal(f.requests.filter(body => body.stream === false).length, 1);
    t.diagnostic(JSON.stringify({ cancelled: true, historyPreserved: true, lateSummaryInstalled: false }));
  } finally { late.resolve(Response.json(completion(summary))); }
});
