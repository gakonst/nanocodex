import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { createWorkersAiResponses } from "../cloudflare/workers-ai-responses.mjs";

test("GLM adapter executes a tool and completes through the real Rust WASM loop", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  let calls = 0;
  let executions = 0;
  const transport = createWorkersAiResponses({
    async run(model, input) {
      assert.equal(model, "@cf/zai-org/glm-5.3");
      assert.equal(input.reasoning_effort, "low");
      calls += 1;
      if (calls === 1) {
        const tool = input.tools.find(tool => tool.function.description.startsWith("runtimeInfo\n"));
        assert.ok(tool, "Rust tool declaration reaches GLM");
        assert.ok(input.messages.some(message => message.content?.includes("Z.ai GLM-5.3")));
        return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
          id: "glm-runtime", type: "function", function: { name: tool.function.name, arguments: "{}" },
        }] } }], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } };
      }
      assert.equal(calls, 2);
      const output = input.messages.find(message => message.role === "tool");
      assert.equal(output.tool_call_id, "glm-runtime");
      assert.match(output.content, /worker-fixture/);
      assert.ok(input.messages.some(message => message.tool_calls?.[0]?.id === "glm-runtime"), "full call history replayed");
      return { choices: [{ finish_reason: "stop", message: { content: "GLM_LOOP_OK" } }], usage: { prompt_tokens: 20, completion_tokens: 5, total_tokens: 25 } };
    },
  });
  const agent = await Agent.create({
    module, model: "@cf/zai-org/glm-5.3", thinking: "low", toolMode: "direct",
    transport: Transport.hostManaged({ ...transport, websocketPreconnect: false,
      createWebSocket() { assert.fail("GLM must never probe WebSocket"); },
    }),
    tools: { runtimeInfo: { description: "Return fixture runtime", parameters: { type: "object", additionalProperties: false },
      handler() { executions += 1; return { runtime: "worker-fixture" }; },
    } },
  });
  try {
    const result = await agent.turn.prompt({ input: "Call runtimeInfo and finish." }).result();
    assert.equal(result.finalMessage, "GLM_LOOP_OK");
    assert.equal(calls, 2);
    assert.equal(executions, 1);
  } finally { agent.dispose(); }
});

for (const model of ["@cf/zai-org/glm-5.3", "kimi-k3", "mimo-v2.6-pro"]) {
  test(`${model} real WASM emits reasoning and answer deltas before provider completion`, { timeout: 5_000 }, async () => {
    const { createGatewayResponses } = await import("../cloudflare/gateway-responses.mjs");
    const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
    let source, requested, reasoning, answer;
    const ready = new Promise(resolve => { requested = resolve; });
    const liveReasoning = new Promise(resolve => { reasoning = resolve; });
    const liveAnswer = new Promise(resolve => { answer = resolve; });
    const stream = new ReadableStream({ start(controller) { source = controller; } });
    const send = (delta, finish_reason = null) => source.enqueue(new TextEncoder().encode(
      `data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason }] })}\n\n`));
    const open = input => { assert.equal(input.stream, true); requested(); return stream; };
    const transport = model.startsWith("@cf/") ? createWorkersAiResponses({ async run(_model, input) { return open(input); } })
      : createGatewayResponses({ provider: "openrouter", model, reasoningEffort: "low", apiKey: "synthetic-key",
        fetch: async (_url, init) => new Response(open(JSON.parse(init.body)), { headers: { "content-type": "text/event-stream" } }) });
    const agent = await Agent.create({ module, model, thinking: "low", tools: [],
      transport: Transport.hostManaged({ ...transport, websocketPreconnect: false,
        createWebSocket() { assert.fail("gateway must use streaming HTTP"); } }) });
    const watch = agent.events.watch();
    watch.onEvent(event => {
      if (event.type === "reasoning.summary.delta") reasoning(event.payload.text);
      if (event.type === "assistant.delta") answer(event.payload.text);
    });
    let finished = false, terminalSent = false;
    const complete = () => {
      if (terminalSent) return;
      terminalSent = true;
      send({}, "stop"); source.enqueue(new TextEncoder().encode("data: [DONE]\n\n"));
    };
    const result = agent.turn.prompt({ input: "Inspect fixture and answer" }).result();
    void result.then(() => { finished = true; }, () => {});
    const live = async promise => {
      let timer;
      try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("live delta was held until completion")), 1_000); })]); }
      finally { clearTimeout(timer); }
    };
    try {
      await ready;
      send(model.startsWith("@cf/") ? { reasoning_content: "Inspect fixture" }
        : { reasoning_details: [{ type: "reasoning.text", text: "Inspect fixture" }] });
      assert.equal(await live(liveReasoning), "Inspect fixture");
      assert.equal(finished, false);
      send({ content: "Answer" });
      assert.equal(await live(liveAnswer), "Answer");
      assert.equal(finished, false);
      complete();
      assert.equal((await result).finalMessage, "Answer");
    } finally {
      complete();
      await result.catch(() => {});
      agent.dispose();
    }
  });
}
