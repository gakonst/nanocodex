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
      assert.equal(init.redirect, "manual");
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

test("telemetry observes actual attempts with no provider content and censors protocol failures", async () => {
  for (const [fetch, outcome, status] of [
    [async () => completion({ content: "private output" }), "success", 200],
    [async () => new Response(secret, { status: 429 }), "http_error", 429],
    [async () => { throw Error(secret); }, "network_error", null],
    [async () => new Response(secret), "protocol_error", 200],
    [async () => Response.json({ error: { message: secret } }), "protocol_error", 200],
    [async () => completion({ tool_calls: [{ function: { name: secret, arguments: "{}" } }] }, "tool_calls"), "protocol_error", 200],
  ]) {
    const observed = [];
    const transport = createGatewayResponses({ ...options, fetch, onRequest(...args) {
      assert.deepEqual(args, []);
      observed.push("start");
      return { headers: status => observed.push(status), finish: result => { observed.push(result); } };
    } });
    const pending = invoke(transport, { input: "private prompt" });
    if (outcome === "success") await pending; else await assert.rejects(pending);
    assert.deepEqual(observed, status === null ? ["start", outcome] : ["start", status, outcome]);
    assert.doesNotMatch(JSON.stringify(observed), /private|synthetic/);
    observed.length = 0;
    await assert.rejects(invoke(transport, { reasoning: { effort: "low" } }));
    assert.deepEqual(observed, []);
  }
});

test("telemetry finalizes cancellation once even when fetch ignores its signal", async () => {
  for (const reason of [new DOMException("private", "AbortError"), new DOMException("private", "TimeoutError")]) {
    const controller = new AbortController();
    let start; const ready = new Promise(resolve => { start = resolve; });
    let complete; const response = new Promise(resolve => { complete = resolve; });
    const outcomes = [];
    const transport = createGatewayResponses({ ...options, fetch: async () => { start(); return response; },
      onRequest: () => ({ headers() {}, finish: outcome => { outcomes.push(outcome); } }) });
    const pending = invoke(transport, {}, controller.signal);
    await ready; controller.abort(reason); await assert.rejects(pending);
    complete(completion({ content: "late" }));
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(outcomes, [reason.name === "TimeoutError" ? "timeout" : "cancelled"]);
  }
});

test("observer exceptions never change a successful generation", async () => {
  for (const onRequest of [() => { throw Error(secret); }, () => ({ headers() { throw Error(secret); }, async finish() { throw Error(secret); } })]) {
    const transport = createGatewayResponses({ ...options, fetch: async () => completion({ content: "ok" }), onRequest });
    assert.equal((await events(await invoke(transport, {}))).at(-1).response.status, "completed");
  }
});

test("telemetry waits for body consumption and classifies body transport errors separately", async () => {
  let release; let notify;
  const headers = new Promise(resolve => { notify = resolve; });
  const observed = [];
  const body = new ReadableStream({ start(controller) { release = () => {
    controller.enqueue(new TextEncoder().encode(JSON.stringify({ choices: [{ message: { content: "ok" }, finish_reason: "stop" }] })));
    controller.close();
  }; } });
  const transport = createGatewayResponses({ ...options, fetch: async () => new Response(body),
    onRequest: () => ({ headers(status) { observed.push(status); notify(); }, finish(outcome) { observed.push(outcome); } }) });
  const pending = invoke(transport, {});
  await headers; assert.deepEqual(observed, [200]);
  release(); await pending; assert.deepEqual(observed, [200, "success"]);
  observed.length = 0;
  const failed = createGatewayResponses({ ...options,
    fetch: async () => new Response(new ReadableStream({ start(controller) { controller.error(new TypeError(secret)); } })),
    onRequest: () => ({ headers(status) { observed.push(status); }, finish(outcome) { observed.push(outcome); } }) });
  await assert.rejects(invoke(failed, {}), error => !String(error).includes(secret));
  assert.deepEqual(observed, [200, "network_error"]);
});

test("OpenRouter single-call mode does not require a parallel-call endpoint", async () => {
  const transport = createGatewayResponses({ ...options, fetch: async (_url, init) => {
    const body = JSON.parse(init.body);
    assert.equal(Object.hasOwn(body, "parallel_tool_calls"), false);
    assert.deepEqual(body.provider, { require_parameters: true });
    assert.deepEqual(body.reasoning, { effort: "high" });
    return completion({ tool_calls: [{ id: "single", function: { name: body.tools[0].function.name, arguments: "{}" } }] }, "tool_calls");
  } });
  const result = await events(await invoke(transport, { parallel_tool_calls: false,
    tools: [{ type: "function", name: "read", parameters: { type: "object" } }], input: "read" }));
  assert.equal(result.at(-1).response.output.length, 1);
});

test("OpenRouter fails closed when single-call mode receives multiple calls", async () => {
  const outcomes = [];
  const transport = createGatewayResponses({ ...options, onRequest: () => ({
    headers() {}, finish(outcome) { outcomes.push(outcome); },
  }), fetch: async (_url, init) => {
    const body = JSON.parse(init.body);
    return completion({ tool_calls: ["one", "two"].map(id => ({ id,
      function: { name: body.tools[0].function.name, arguments: "{}" } })) }, "tool_calls");
  } });
  await assert.rejects(invoke(transport, { parallel_tool_calls: false,
    tools: [{ type: "function", name: "read", parameters: { type: "object" } }], input: "read" }), /Gateway Responses/);
  assert.deepEqual(outcomes, ["protocol_error"]);
});

test("explicit parallel mode still requires provider support", async () => {
  const transport = createGatewayResponses({ ...options, fetch: async (_url, init) => {
    const body = JSON.parse(init.body);
    assert.equal(body.parallel_tool_calls, true);
    assert.deepEqual(body.provider, { require_parameters: true });
    return completion({ content: "done" });
  } });
  await invoke(transport, { parallel_tool_calls: true, input: "read" });
});

for (const provider of ["openrouter", "vercel"]) {
  test(`${provider} rejects redirects without forwarding server credentials`, async () => {
    let calls = 0;
    const outcomes = [];
    const transport = createGatewayResponses({ ...options, provider,
      onRequest: () => ({ headers() {}, finish(outcome) { outcomes.push(outcome); } }),
      fetch: async (_url, init) => {
        calls++;
        // workerd supports manual and follow, but rejects redirect: error.
        assert.equal(init.redirect, "manual");
        return new Response(null, { status: 307, headers: { location: "https://other.invalid/" } });
      },
    });
    await assert.rejects(invoke(transport, {}), /Gateway Responses/);
    assert.equal(calls, 1);
    assert.deepEqual(outcomes, ["http_error"]);
  });
}
