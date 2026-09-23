import test from "node:test";
import assert from "node:assert/strict";
import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";

const tools = [{ type: "namespace", name: "functions", tools: [{ type: "custom", name: "exec" }] },
  { type: "namespace", name: "other", tools: [{ type: "custom", name: "exec" }] }];
for (const [name, code] of [
  ["functions.tool_0", "namespace_wire"], ["functions_exec", "namespace_flat"], ["functions__exec", "namespace_flat"],
  ["tool_0tool_0", "repeated"], ["functions.execfunctions.exec", "repeated"], ["execexecexec", "repeated"],
  ["exec", "ambiguous"], ["tool_99", "unregistered_wire"], ["multi_tool_use.parallel", "parallel_wrapper"],
  ["synthetic-secret", null], ["functions.synthetic-secret", null], ["synthetic-secret".repeat(100), null],
]) test(`unresolved tool name produces only local diagnostic ${code ?? "unknown"}: ${name.length}`, async () => {
  let requests = 0;
  const outcomes = [], exposed = [];
  const transport = createGatewayResponses({ provider: "openrouter", model: "mimo-v2.6-pro", reasoningEffort: "high", apiKey: "synthetic-key",
    onRequest: () => ({ finish(outcome) { outcomes.push(outcome); } }), fetch: async () => {
      requests++;
      // Exercise real fragment accumulation, including a repeated complete name.
      const records = [name.slice(0, 3), name.slice(3)].map((fragment, index) => ({ choices: [{ index: 0,
        delta: { tool_calls: [{ index: 0, ...(index === 0 ? { id: "synthetic-call", type: "function" } : {}), function: { name: fragment, arguments: index === 0 ? '{"input":"synthetic-secret"}' : "" } }] }, finish_reason: null }] }));
      records.push({ choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] });
      return new Response(records.map(value => `data: ${JSON.stringify(value)}\n\n`).join("") + "data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
    } });
  await assert.rejects(async () => {
    const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
      authorization: "host_managed", body: JSON.stringify({ tools, input: "Read fixture", stream: true }),
    });
    for await (const bytes of response.body) exposed.push(new TextDecoder().decode(bytes));
  }, { message: `Responses: invalid provider stream\nProtocol invariant: normalize_tool_alias${code ? `_${code}` : ""}` });
  assert.doesNotMatch(exposed.join(""), /synthetic-secret|response\.output_item\.added/);
  assert.equal(requests, 1);
  assert.deepEqual(outcomes, ["protocol_error"]);
});
