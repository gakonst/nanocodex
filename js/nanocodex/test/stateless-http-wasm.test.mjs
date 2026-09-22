import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { createWorkersAiResponses } from "../cloudflare/workers-ai-responses.mjs";

test("stateless GPT transport executes a tool with full replay through the real Rust WASM loop", { timeout: 30_000 }, async () => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  let calls = 0;
  let executions = 0;
  const transport = createWorkersAiResponses({
    async run(model, input) {
      assert.equal(model, "gpt-5.6-sol");
      assert.equal(input.reasoning_effort, "low");
      calls += 1;
      if (calls === 1) {
        const tool = input.tools.find(tool => tool.function.description.startsWith("runtimeInfo\n"));
        assert.ok(tool, "Rust tool declaration reaches GPT");
        return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
          id: "gpt-runtime", type: "function", function: { name: tool.function.name, arguments: "{}" },
        }] } }], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } };
      }
      assert.equal(calls, 2);
      const output = input.messages.find(message => message.role === "tool");
      assert.equal(output.tool_call_id, "gpt-runtime");
      assert.match(output.content, /worker-fixture/);
      assert.ok(input.messages.some(message => message.tool_calls?.[0]?.id === "gpt-runtime"), "full call history replayed");
      return { choices: [{ finish_reason: "stop", message: { content: "GPT_LOOP_OK" } }], usage: { prompt_tokens: 20, completion_tokens: 5, total_tokens: 25 } };
    },
  }, { model: "gpt-5.6-sol" });
  const agent = await Agent.create({
    module, model: "gpt-5.6-sol", thinking: "low", toolMode: "direct",
    transport: Transport.hostManaged({ ...transport, stateless: true,
      websocketPreconnect: true, websocketWarmup: true, websocketUrl: "wss://stateless.invalid/responses",
      createResponse(endpoint, sessionId, request) {
        const body = JSON.parse(request.body);
        assert.equal(body.store, false);
        assert.equal(body.previous_response_id, undefined);
        return transport.createResponse(endpoint, sessionId, request);
      },
      createWebSocket() { assert.fail("stateless GPT must never probe WebSocket"); },
    }),
    tools: { runtimeInfo: { description: "Return fixture runtime", parameters: { type: "object", additionalProperties: false },
      handler() { executions += 1; return { runtime: "worker-fixture" }; },
    } },
  });
  try {
    const result = await agent.turn.prompt({ input: "Call runtimeInfo and finish." }).result();
    assert.equal(result.finalMessage, "GPT_LOOP_OK");
    assert.equal(calls, 2);
    assert.equal(executions, 1);
  } finally { agent.dispose(); }
});
