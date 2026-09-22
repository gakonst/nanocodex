import assert from "node:assert/strict";
import { test } from "node:test";
import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";
const models = ["@cf/zai-org/glm-5.3", "gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];
const secret = "synthetic-server-only-key";
const options = { provider: "openrouter", model: models[0], reasoningEffort: "high", apiKey: secret };
const completion = (message, finish_reason = "stop") => Response.json({ choices: [{ message, finish_reason }] });
const invoke = (transport, body, signal) => transport.createResponse(`${transport.apiBaseUrl}/responses`, "session", {
  authorization: "host_managed", body: JSON.stringify(body), signal,
});
const events = async response => (await response.text()).trim().split("\n\n").map(frame => JSON.parse(frame.split("\ndata: ")[1]));
for (const provider of ["openrouter", "vercel"]) for (const model of models) {
  test(`${provider}/${model} pins routing and round-trips namespaced custom tools`, async () => {
    const requests = [];
    const transport = createGatewayResponses({ ...options, provider, model, fetch: async (url, init) => {
      assert.equal(url, provider === "openrouter" ? "https://openrouter.ai/api/v1/chat/completions" : "https://ai-gateway.vercel.sh/v1/chat/completions");
      assert.equal(init.redirect, "error");
      assert.equal(init.headers.authorization, `Bearer ${secret}`);
      const body = JSON.parse(init.body); requests.push(body);
      assert.equal(body.model, model === models[0] ? (provider === "openrouter" ? "z-ai/glm-5.3" : "zai/glm-5.3") : `openai/${model}`);
      assert.equal(body.stream, false); assert.equal(body.models, undefined);
      if (provider === "openrouter") { assert.deepEqual(body.reasoning, { effort: "high" }); assert.deepEqual(body.provider, { require_parameters: true }); }
      else assert.equal(body.reasoning_effort, "high");
      if (requests.length === 1) return completion({ tool_calls: [{ id: "call", function: { name: body.tools[0].function.name, arguments: JSON.stringify({ input: "text(42)" }) } }] }, "tool_calls");
      assert.equal(body.messages.at(-1).role, "tool"); assert.equal(body.messages.at(-1).content, "42");
      return completion({ content: "done" });
    } });
    assert.equal(transport.stateless, true);
    const tools = [{ type: "namespace", name: "functions", tools: [{ type: "custom", name: "exec" }] }];
    const first = await events(await invoke(transport, { model, tools, input: "run", reasoning: { effort: "high" } }));
    const response = first.at(-1).response;
    assert.equal(response.model, model); assert.equal(response.end_turn, false);
    assert.equal(response.output[0].name, "exec"); assert.equal(response.output[0].namespace, "functions");
    const second = await events(await invoke(transport, { model, tools, input: [
      { role: "user", content: "run" }, ...response.output,
      { type: "custom_tool_call_output", call_id: "call", output: "42" },
    ] }));
    assert.equal(second.at(-1).response.end_turn, true);
    assert.equal(second.at(-1).response.model, model);
  });
}
test("invalid overrides, compaction and non-host authorization never dispatch", async () => {
  let calls = 0;
  const transport = createGatewayResponses({ ...options, fetch: async () => { calls++; return completion({ content: "bad" }); } });
  for (const body of [{ model: models[1] }, { reasoning: { effort: "low" } }, { previous_response_id: "opaque" }, { context_management: [{}] }, { input: [{ type: "compaction", encrypted_content: "opaque" }] }]) {
    await assert.rejects(invoke(transport, body), /Gateway Responses/);
  }
  await assert.rejects(transport.createResponse("https://attacker.invalid/responses", "s", { authorization: "host_managed", body: "{}" }));
  await assert.rejects(transport.createResponse(`${transport.apiBaseUrl}/responses`, "s", { authorization: "none", body: "{}" }));
  assert.equal(calls, 0);
});
test("errors redact upstream bodies, thrown fetch errors and malformed completions", async () => {
  for (const fetch of [
    async () => new Response(secret, { status: 401, statusText: secret }),
    async () => { throw new Error(secret); },
    async () => new Response(secret),
    async () => completion({ tool_calls: [{ function: { name: secret, arguments: secret } }] }, "tool_calls"),
  ]) {
    const transport = createGatewayResponses({ ...options, fetch });
    await assert.rejects(invoke(transport, {}), error => !String(error).includes(secret) && /Gateway Responses/.test(String(error)));
  }
});
test("cancellation works before and during dispatch and forwards the signal", async () => {
  let calls = 0; let observed; let started;
  const ready = new Promise(resolve => { started = resolve; });
  const transport = createGatewayResponses({ ...options, fetch: async (_url, init) => {
    calls++; observed = init.signal; started(); return new Promise(() => {});
  } });
  const before = new AbortController(); before.abort();
  await assert.rejects(invoke(transport, {}, before.signal), { name: "AbortError" }); assert.equal(calls, 0);
  const during = new AbortController(); const pending = invoke(transport, {}, during.signal);
  await ready; during.abort(); await assert.rejects(pending, { name: "AbortError" });
  assert.equal(observed, during.signal); assert.equal(observed.aborted, true);
});
test("gateway reasoning text is emitted with the canonical response identity", async () => {
  const transport = createGatewayResponses({ ...options, model: models[1], fetch: async () => completion({ reasoning: "considered", content: "done" }) });
  const stream = await events(await invoke(transport, {}));
  assert.equal(stream.at(-1).response.output[0].content[0].text, "considered");
  for (const event of stream.filter(event => event.response)) assert.equal(event.response.model, models[1]);
});
