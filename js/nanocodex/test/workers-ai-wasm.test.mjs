import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { Agent, Transport } from "../host/index.mjs";
import { toolResult } from "../../nanocodex-tools/runtime/code-runtime.mjs";
import { createWorkersAiResponses } from "../cloudflare/workers-ai-responses.mjs";
import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";

const screenshot = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgAAIAAAUAAXpeqz8AAAAASUVORK5CYII=";

function deferred() {
  let resolve;
  const promise = new Promise(r => { resolve = r; });
  return { promise, resolve };
}

async function within(promise, label, milliseconds = 3_000) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} did not settle within ${milliseconds}ms`)), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

test("GLM real WASM preserves screenshot history, accepts steering and completes using text without replay", { timeout: 15_000 }, async t => {
  const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const screenshotStarted = deferred();
  const releaseScreenshot = deferred();
  const trace = [];
  const toolResults = [];
  let calls = 0, screenshots = 0, textReads = 0;
  let fixtureError;
  const toolCall = (input, name, id) => {
    const tool = input.tools.find(tool => tool.function.description.startsWith(`${name}\n`));
    assert.ok(tool, `${name} declaration reaches GLM`);
    return { choices: [{ finish_reason: "tool_calls", message: { content: null, tool_calls: [{
      id, type: "function", function: { name: tool.function.name, arguments: "{}" },
    }] } }], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } };
  };
  const transport = createWorkersAiResponses({
    async run(model, input) {
      try {
        assert.equal(model, "@cf/zai-org/glm-5.3");
        assert.equal(input.reasoning_effort, "low");
        calls += 1;
        const encoded = JSON.stringify(input.messages);
        assert.doesNotMatch(encoded, /iVBORw0KGgo/, "GLM requests contain no screenshot bytes");
        for (const message of input.messages) {
          assert.equal(typeof message.content === "string" || message.content === null, true,
            "GLM receives text messages, never image content parts");
        }
        trace.push({ inference: calls, toolCallIds: input.messages.filter(message => message.role === "tool").map(message => message.tool_call_id) });
        if (calls === 1) {
          assert.ok(input.messages.some(message => message.content?.includes("Z.ai GLM-5.3")));
          return toolCall(input, "captureScreen", "glm-screen");
        }
        const output = input.messages.find(message => message.role === "tool" && message.tool_call_id === "glm-screen");
        assert.ok(output, "screenshot call retains its corresponding tool result");
        assert.match(output.content, /Screen capture completed/);
        assert.match(output.content, /Image omitted: GLM-5\.3 cannot/);
        assert.match(output.content, /vision.capable subagent/i);
        assert.match(encoded, /STEER_TEXT: use the text inspector and report the button label/);
        assert.ok(input.messages.some(message => message.tool_calls?.some(call => call.id === "glm-screen")), "original call remains in provider history");
        if (calls === 2) return toolCall(input, "inspectText", "glm-text");
        assert.equal(calls, 3, "only screenshot, text fallback and completion inferences are needed");
        const text = input.messages.find(message => message.role === "tool" && message.tool_call_id === "glm-text");
        assert.match(text?.content, /Continue/);
        return { choices: [{ finish_reason: "stop", message: { content: "The button label is Continue." } }],
          usage: { prompt_tokens: 20, completion_tokens: 5, total_tokens: 25 } };
      } catch (error) { fixtureError ??= error; throw error; }
    },
  });
  const agent = await Agent.create({
    module, model: "@cf/zai-org/glm-5.3", thinking: "low", toolMode: "direct",
    transport: Transport.hostManaged({ ...transport, websocketPreconnect: false,
      createWebSocket() { assert.fail("GLM must never probe WebSocket"); },
    }),
    tools: {
      captureScreen: { description: "Capture the fixture screen", parameters: { type: "object", additionalProperties: false },
        async handler() {
          screenshots += 1;
          screenshotStarted.resolve();
          await releaseScreenshot.promise;
          return toolResult([
            { type: "input_text", text: "Screen capture completed" },
            { type: "input_image", image_url: `data:image/png;base64,${screenshot}`, detail: "original" },
          ], { content: [{ type: "text", text: "Screen capture completed" },
            { type: "image", mimeType: "image/png", data: screenshot }] });
        },
      },
      inspectText: { description: "Read accessible text on the fixture screen", parameters: { type: "object", additionalProperties: false },
        handler() { textReads += 1; return { buttonLabel: "Continue" }; },
      },
    },
  });
  const stop = agent.events.watch().onEvent(event => {
    if (event.type === "tool.result") toolResults.push(event.payload);
  });
  const turn = agent.turn.prompt({ input: "Capture the screen once and identify the button label." });
  const result = turn.result();
  void result.catch(() => {});
  try {
    await within(Promise.race([screenshotStarted.promise, result.then(() => assert.fail("turn finished before screenshot"))]), "screenshot admission", 8_000);
    await within(turn.steer({ input: "STEER_TEXT: use the text inspector and report the button label.", messageId: "glm-screen-steer" }), "steering");
    const startedAt = performance.now();
    releaseScreenshot.resolve();
    const completed = await within(result, "GLM screenshot recovery");
    assert.equal(completed.finalMessage, "The button label is Continue.");
    assert.equal(calls, 3);
    assert.equal(screenshots, 1);
    assert.equal(textReads, 1);
    const receipt = toolResults.find(output => output.call_id === "glm-screen");
    assert.ok(receipt, "public tool receipt is retained");
    assert.deepEqual(receipt.structured_result.content, [
      { type: "text", text: "Screen capture completed" },
      { type: "image", mimeType: "image/png", data: screenshot },
    ]);
    const { history } = await agent.session.context();
    const retained = history.find(item => item.type === "function_call_output" && item.call_id === "glm-screen");
    assert.ok(Array.isArray(retained?.output), "public session history retains multimodal tool output");
    assert.ok(retained.output.some(part => part.type === "input_image" && part.image_url === `data:image/png;base64,${screenshot}`),
      "original screenshot remains available for a vision-capable continuation");
    assert.ok(retained.output.some(part => part.type === "input_text" && part.text.includes("Screen capture completed")));
    assert.doesNotMatch(JSON.stringify(retained), /Image omitted/, "adapter notice does not replace retained history");
    t.diagnostic(JSON.stringify({ trace, screenshots, textReads, steeringAccepted: true, imageRetained: true,
      recoveryMs: Math.round(performance.now() - startedAt), finalMessage: completed.finalMessage }));
  } catch (error) {
    t.diagnostic(JSON.stringify({ trace, screenshots, textReads }));
    throw fixtureError ?? error;
  } finally {
    releaseScreenshot.resolve();
    stop();
    await turn.cancel().catch(() => {});
    await result.catch(() => {});
    await agent.session.shutdown().catch(() => {});
  }
});

for (const provider of ["workers-ai", "openrouter", "vercel"]) {
  test(`${provider}: GLM real WASM terminates unsupported user image input after one request`, { timeout: 10_000 }, async t => {
    const module = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
    let requests = 0, inferences = 0;
    const statuses = [];
    const infer = async () => {
      inferences += 1;
      assert.fail("unsupported user image must be rejected before provider inference");
    };
    const transport = provider === "workers-ai" ? createWorkersAiResponses({ run: infer })
      : createGatewayResponses({ provider, model: "@cf/zai-org/glm-5.3", reasoningEffort: "low",
        apiKey: "synthetic-key", fetch: infer });
    const agent = await Agent.create({ module, model: "@cf/zai-org/glm-5.3", thinking: "low", tools: [],
      transport: Transport.hostManaged({ ...transport, websocketPreconnect: false,
        createWebSocket() { assert.fail("GLM must never probe WebSocket"); },
        async createResponse(...args) {
          requests += 1;
          const response = await transport.createResponse(...args);
          statuses.push(response.status);
          return response;
        },
      }),
    });
    const turn = agent.turn.prompt({ input: [
      { type: "text", text: "Describe this screenshot." },
      { type: "image", image_url: `data:image/png;base64,${screenshot}` },
    ] });
    const result = turn.result();
    const startedAt = performance.now();
    try {
      await assert.rejects(within(result, "unsupported user image failure"), /GLM-5\.3 accepts text only/);
      assert.equal(requests, 1, "deterministic unsupported input is terminal, never retried");
      assert.equal(inferences, 0);
      assert.deepEqual(statuses, [400]);
      t.diagnostic(JSON.stringify({ provider, requests, inferences, statuses, terminal: true,
        failureMs: Math.round(performance.now() - startedAt) }));
    } finally {
      await turn.cancel().catch(() => {});
      await result.catch(() => {});
      await agent.session.shutdown().catch(() => {});
    }
  });
}

for (const model of ["@cf/zai-org/glm-5.3", "kimi-k3", "mimo-v2.6-pro"]) {
  test(`${model} real WASM emits reasoning and answer deltas before provider completion`, { timeout: 5_000 }, async () => {
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
