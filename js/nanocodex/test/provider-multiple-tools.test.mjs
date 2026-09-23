import test from "node:test";
import assert from "node:assert/strict";
import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";

const tools = [{ type: "namespace", name: "functions", tools: [
  { type: "function", name: "exec_command", parameters: { type: "object" } },
  { type: "custom", name: "exec" },
] }];
const calls = [
  { id: "call-shell", type: "function", function: { name: "tool_0", arguments: '{"cmd":"printf fixture"}' } },
  { id: "call-code", type: "function", function: { name: "tool_1", arguments: '{"input":"text(42)"}' } },
];
const events = async response => (await response.text()).trim().split("\n\n").map(frame => JSON.parse(frame.split("\ndata: ")[1]));
const sse = records => new Response(records.map(value => `data: ${typeof value === "string" ? value : JSON.stringify(value)}\n\n`).join(""),
  { headers: { "content-type": "text/event-stream" } });

// The request bit is a generation preference. Every returned call must retain
// its identity through validation and history; execution policy lives in the host.
for (const provider of ["openrouter", "vercel", "cloudflare"]) {
  for (const model of provider === "cloudflare" ? ["gpt-6-sol"] : ["kimi-k3", "mimo-v2.6-pro", "@cf/zai-org/glm-5.3"]) {
    for (const stream of [false, true]) test(`${provider}/${model}/${stream ? "stream" : "buffered"}: multiple calls survive false parallel preference and replay`, async () => {
      let requests = 0;
      const outcomes = [];
      const transport = createGatewayResponses({ provider, model, reasoningEffort: "high", apiKey: "synthetic-key",
        ...(provider === "cloudflare" ? { accountId: "a".repeat(32) } : {}),
        onRequest: () => ({ finish(outcome) { outcomes.push(outcome); } }),
        fetch: async (_url, init) => {
          const body = JSON.parse(init.body);
          requests++;
          if (provider !== "cloudflare") assert.equal(Object.hasOwn(body, "parallel_tool_calls"), provider === "vercel" && model === "@cf/zai-org/glm-5.3");
          else assert.equal(body.parallel_tool_calls, false);
          if (requests === 2) {
            if (provider === "cloudflare") {
              assert.deepEqual(body.input.filter(item => item.type === "function_call").map(item => item.call_id), calls.map(call => call.id));
              assert.deepEqual(body.input.filter(item => item.type === "function_call_output").map(item => [item.call_id, item.output]), [["call-shell", "fixture"], ["call-code", "42"]]);
            } else {
              assert.deepEqual(body.messages.find(message => message.tool_calls).tool_calls, calls);
              assert.deepEqual(body.messages.filter(message => message.role === "tool").map(message => [message.tool_call_id, message.content]), [["call-shell", "fixture"], ["call-code", "42"]]);
            }
          }
          if (provider === "cloudflare") {
            const result = { object: "response", status: "completed", output: requests === 1
              ? calls.map((call, index) => ({ type: "function_call", id: `item-${index}`, call_id: call.id, status: "completed", ...call.function }))
              : [{ type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text: "done" }] }] };
            return stream ? sse([{ type: "response.completed", response: result }]) : Response.json(result);
          }
          const message = requests === 1 ? { tool_calls: calls } : { content: "done" };
          const finish_reason = requests === 1 ? "tool_calls" : "stop";
          return stream ? sse([{ choices: [{ index: 0, delta: requests === 1
            ? { tool_calls: calls.map((call, index) => ({ ...call, index })) } : message, finish_reason: null }] },
          { choices: [{ index: 0, delta: {}, finish_reason }] }, "[DONE]"])
            : Response.json({ choices: [{ message, finish_reason }] });
        } });
      const invoke = input => transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
        authorization: "host_managed", body: JSON.stringify({ input, tools, stream, parallel_tool_calls: false }),
      });
      const first = await events(await invoke("Read fixture"));
      const output = first.at(-1).response.output;
      assert.deepEqual(output.map(item => [item.type, item.call_id, item.namespace, item.name]), [
        ["function_call", "call-shell", "functions", "exec_command"], ["custom_tool_call", "call-code", "functions", "exec"],
      ]);
      assert.equal(output[0].arguments, calls[0].function.arguments);
      assert.equal(output[1].input, "text(42)");
      assert.deepEqual(first.filter(event => event.type === "response.output_item.done").map(event => event.item.call_id), calls.map(call => call.id));
      const second = await events(await invoke([{ role: "user", content: "Read fixture" }, ...output,
        { type: "function_call_output", call_id: "call-shell", output: "fixture" },
        { type: "custom_tool_call_output", call_id: "call-code", output: "42" }]));
      assert.equal(second.at(-1).response.output[0].content[0].text, "done");
      assert.equal(requests, 2, "one provider attempt per model response; no retry or fallback");
      assert.deepEqual(outcomes, ["success", "success"]);
    });
  }
}

for (const model of ["kimi-k3", "mimo-v2.6-pro"]) for (const stream of [false, true]) {
  test(`${model}/${stream ? "stream" : "buffered"}: an invalid sibling never exposes a partial tool batch`, async () => {
    for (const invalid of [
      { ...calls[1], id: calls[0].id },
      { ...calls[1], function: { name: "unadvertised", arguments: "{}" } },
      { ...calls[1], function: { name: "tool_1", arguments: '{"input":42}' } },
    ]) {
      let requests = 0;
      const outcomes = [], exposed = [];
      const transport = createGatewayResponses({ provider: "openrouter", model, reasoningEffort: "high", apiKey: "synthetic-key",
        onRequest: () => ({ finish(outcome) { outcomes.push(outcome); } }), fetch: async () => {
          requests++;
          const batch = [calls[0], invalid];
          return stream ? sse([{ choices: [{ index: 0, delta: { tool_calls: batch.map((call, index) => ({ ...call, index })) }, finish_reason: "tool_calls" }] }, "[DONE]"])
            : Response.json({ choices: [{ message: { tool_calls: batch }, finish_reason: "tool_calls" }] });
        } });
      await assert.rejects(async () => {
        const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
          authorization: "host_managed", body: JSON.stringify({ input: "Read fixture", tools, stream, parallel_tool_calls: false }),
        });
        for await (const bytes of response.body) exposed.push(new TextDecoder().decode(bytes));
      }, /Gateway Responses|invalid provider stream/);
      assert.doesNotMatch(exposed.join(""), /response\.output_item\.added|response\.function_call_arguments|response\.custom_tool_call_input/);
      assert.equal(requests, 1, "invalid batch must not be retried");
      assert.deepEqual(outcomes, ["protocol_error"]);
    }
  });
}
