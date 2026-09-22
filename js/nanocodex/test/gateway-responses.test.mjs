import assert from "node:assert/strict";
import { test } from "node:test";
import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";
const models = ["@cf/zai-org/glm-5.3", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna"];
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

const nativeResponse = (output, extra = {}) => ({ object: "response", status: "completed", output, ...extra });
const nativeText = text => ({ type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text }] });
const bindingOptions = { provider: "cloudflare", model: "gpt-6-astra", reasoningEffort: "high" };
for (const wire of ["binding", "rest"]) for (const model of models.slice(1)) {
  test(`cloudflare/${wire}/${model} uses native Responses and round-trips all managed tools`, async () => {
    const observed = [], requests = [];
    const declared = [
      { type: "namespace", name: "functions", tools: [{ type: "custom", name: "exec", format: { syntax: "lark", definition: "start: /.+/" } }] },
      { type: "function", name: "read", parameters: { type: "object", properties: { path: { type: "string" } } } },
      { type: "tool_search", execution: "client", parameters: { type: "object", properties: { query: { type: "string" } } } },
    ];
    const discovered = [{ type: "namespace", name: "remote", tools: [{ type: "function", name: "read", parameters: { type: "object" } }] }];
    const run = async (upstream, input) => {
        assert.equal(upstream, `openai/${model}`); requests.push(input);
        assert.equal(input.messages, undefined); assert.equal(input.model, undefined);
        assert.equal(input.stream, false); assert.equal(input.store, false);
        assert.deepEqual(input.reasoning, { effort: "high" });
        assert.equal(input.reasoning_effort, undefined); assert.equal(input.max_output_tokens, 512);
        assert.equal(input.max_completion_tokens, undefined);
        assert.ok(input.tools.every(tool => tool.type === "function" && !tool.function && tool.strict === false));
        if (requests.length === 1) {
          assert.equal(input.tool_choice, "auto");
          return nativeResponse([
            { type: "reasoning", summary: [{ type: "summary_text", text: "plan" }], content: [{ type: "reasoning_text", text: "considered" }], encrypted_content: "opaque" },
            ...[JSON.stringify({ input: "text(42)\n" }), '{"path":"a"}', '{"query":"read"}'].map((args, i) => ({
              type: "function_call", call_id: `call_${i}`, name: input.tools[i].name, arguments: args, status: "completed",
            })),
          ], { usage: { input_tokens: 50, output_tokens: 30, total_tokens: 80,
            input_tokens_details: { cached_tokens: 20 }, output_tokens_details: { reasoning_tokens: 12 } } });
        }
        assert.ok(input.input.some(item => item.role === "assistant" && item.content === "plan\nconsidered"));
        assert.equal(JSON.stringify(input).includes("opaque"), false);
        const calls = input.input.filter(item => item.type === "function_call");
        assert.deepEqual(calls.slice(0, 3).map(call => call.name), input.tools.slice(0, 3).map(tool => tool.name));
        assert.equal(calls[0].arguments, JSON.stringify({ input: "text(42)\n" }));
        assert.deepEqual(input.input.filter(item => item.type === "function_call_output").slice(0, 3).map(item => item.output),
          ["42", "contents", JSON.stringify({ tools: discovered })]);
        assert.equal(input.tools.length, 4);
        if (requests.length === 2) return nativeResponse([{ type: "function_call", call_id: "found", name: input.tools[3].name, arguments: "{}" }]);
        assert.equal(calls[3].name, input.tools[3].name);
        assert.equal(input.input.at(-1).output, "remote contents");
        return nativeResponse([nativeText("done")]);
    };
    const transport = createGatewayResponses({ ...bindingOptions, model,
      onRequest: () => ({ headers(status) { observed.push(status); }, finish(outcome) { observed.push(outcome); } }),
      ...(wire === "binding" ? { ai: { run } } : { accountId: "a".repeat(32), apiKey: secret,
        fetch: async (url, init) => {
          assert.equal(url, `https://api.cloudflare.com/client/v4/accounts/${"a".repeat(32)}/ai/v1/responses`);
          assert.equal(init.redirect, "manual"); assert.equal(init.headers.authorization, `Bearer ${secret}`);
          const { model: upstream, ...input } = JSON.parse(init.body);
          return Response.json(await run(upstream, input));
        } }),
    });
    const common = { model, tools: declared, max_output_tokens: 512 };
    const first = (await events(await invoke(transport, { ...common, input: "run", tool_choice: "auto" }))).at(-1).response;
    assert.equal(first.model, model); assert.equal(first.end_turn, false);
    assert.deepEqual(first.output.map(item => item.type), ["reasoning", "custom_tool_call", "function_call", "tool_search_call"]);
    assert.equal(first.output[1].namespace, "functions"); assert.equal(first.output[1].input, "text(42)\n");
    assert.equal(first.output[2].name, "read"); assert.equal(first.output[3].execution, "client");
    assert.deepEqual(first.usage, { input_tokens: 50, output_tokens: 30, total_tokens: 80,
      input_tokens_details: { cached_tokens: 20 }, output_tokens_details: { reasoning_tokens: 12 } });
    const history = [{ role: "user", content: "run" }, ...first.output,
      { type: "custom_tool_call_output", call_id: "call_0", output: "42" },
      { type: "function_call_output", call_id: "call_1", output: "contents" },
      { type: "tool_search_output", call_id: "call_2", tools: discovered }];
    const second = (await events(await invoke(transport, { ...common, input: history }))).at(-1).response;
    assert.equal(second.output[0].name, "read"); assert.equal(second.output[0].namespace, "remote");
    const third = (await events(await invoke(transport, { ...common, input: [...history, ...second.output,
      { type: "function_call_output", call_id: "found", output: "remote contents" }] }))).at(-1).response;
    assert.equal(third.output[0].content[0].text, "done"); assert.equal(third.end_turn, true);
    assert.deepEqual(observed, wire === "binding" ? ["success", "success", "success"] : [200, "success", 200, "success", 200, "success"]);
  });
}

test("Cloudflare validates model, effort, full history and hosted tools before dispatch", async () => {
  let calls = 0;
  const ai = { async run() { calls++; return nativeResponse([nativeText("ok")]); } };
  assert.throws(() => createGatewayResponses({ ...bindingOptions, model: models[0], ai }));
  assert.throws(() => createGatewayResponses(bindingOptions));
  const transport = createGatewayResponses({ ...bindingOptions, ai });
  for (const body of [
    { model: "gpt-6-sol" }, { reasoning: { effort: "low" } },
    { input: [{ type: "configuration_update", reasoning: { effort: "medium" } }] },
    { previous_response_id: "opaque" }, { context_management: [{}] },
    { input: [{ type: "compaction", encrypted_content: "opaque" }] },
    { tools: [{ type: "web_search" }] }, { tools: [{ type: "tool_search", execution: "server" }] },
  ]) await assert.rejects(invoke(transport, body), /Gateway Responses/);
  await assert.rejects(transport.createResponse("https://other.invalid/responses", "s", { authorization: "host_managed", body: "{}" }));
  await assert.rejects(transport.createResponse(`${transport.apiBaseUrl}/responses`, "s", { authorization: "none", body: "{}" }));
  assert.equal(calls, 0);
});

test("Cloudflare fails closed and sanitizes binding errors without retries or fallback", async () => {
  const tool = { type: "function_call", call_id: "one", name: "tool_0", arguments: "{}" };
  for (const [run, outcome] of [
    [async () => { throw Error(secret); }, "network_error"],
    ...[
      { error: { message: secret } }, { choices: [] }, nativeResponse([], { status: "failed", error: { message: secret } }),
      nativeResponse([{ type: "web_search_call", status: "completed" }]),
      nativeResponse([{ ...tool, name: secret }]), nativeResponse([{ ...tool, arguments: secret }]),
      nativeResponse([{ ...tool, call_id: undefined }]), nativeResponse([tool, { ...tool, call_id: "two" }]),
      nativeResponse([tool], { status: "incomplete", incomplete_details: { reason: "max_output_tokens" } }),
      nativeResponse([{ ...nativeText("x"), content: [{ type: "refusal", refusal: secret }] }]),
      nativeResponse([nativeText("x")], { usage: { input_tokens: secret } }),
      nativeResponse([], { status: "incomplete", incomplete_details: { reason: secret } }),
    ].map(value => [async () => value, "protocol_error"]),
  ]) {
    let calls = 0; const observed = [];
    const transport = createGatewayResponses({ ...bindingOptions,
      ai: { async run(...args) { calls++; return run(...args); } },
      onRequest: () => ({ headers(status) { observed.push(status); }, finish(outcome) { observed.push(outcome); } }),
    });
    await assert.rejects(invoke(transport, { parallel_tool_calls: false, tools: [{ type: "function", name: "read" }] }),
      error => /Gateway Responses/.test(String(error)) && !String(error).includes(secret));
    assert.equal(calls, 1); assert.deepEqual(observed, [outcome]);
  }
});

for (const reason of ["max_output_tokens", "content_filter"]) {
  test(`Cloudflare preserves incomplete ${reason} and buffered SSE`, async () => {
    const transport = createGatewayResponses({ ...bindingOptions, ai: { async run() {
      return nativeResponse([{ ...nativeText("partial"), status: "incomplete" }], { status: "incomplete", incomplete_details: { reason } });
    } } });
    const stream = await events(await invoke(transport, {}));
    assert.equal(stream[0].type, "response.created");
    assert.ok(stream.some(event => event.type === "response.output_text.delta" && event.delta === "partial"));
    assert.equal(stream.at(-1).type, "response.incomplete");
    assert.equal(stream.at(-1).response.end_turn, false);
    assert.deepEqual(stream.at(-1).response.incomplete_details, { reason });
  });
}

test("Cloudflare cancellation stops waiting and finalizes telemetry once without headers", async () => {
  for (const name of ["AbortError", "TimeoutError"]) {
    let calls = 0, start, release;
    const started = new Promise(resolve => { start = resolve; });
    const result = new Promise(resolve => { release = resolve; });
    const observed = [];
    const transport = createGatewayResponses({ ...bindingOptions, ai: { async run() { calls++; start(); return result; } },
      onRequest: () => ({ headers(status) { observed.push(status); }, finish(outcome) { observed.push(outcome); } }),
    });
    const before = new AbortController(); before.abort();
    await assert.rejects(invoke(transport, {}, before.signal), { name: "AbortError" }); assert.equal(calls, 0);
    const during = new AbortController(); const pending = invoke(transport, {}, during.signal);
    await started; during.abort(new DOMException("cancelled", name));
    await assert.rejects(pending, { name });
    release(nativeResponse([nativeText("late")]));
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(calls, 1); assert.deepEqual(observed, [name === "TimeoutError" ? "timeout" : "cancelled"]);
  }
});

test("Cloudflare replays portable text and historical tools in native Responses order", async () => {
  const transport = createGatewayResponses({ ...bindingOptions, ai: { async run(_model, payload) {
    assert.deepEqual(payload.input, [
      { role: "system", content: "instructions" },
      { role: "system", content: "developer text" },
      { role: "user", content: JSON.stringify({ author: "peer", recipient: "parent", message: "update" }) },
      { role: "assistant", content: "historic thought" },
      { role: "assistant", content: "historic answer" },
      { type: "function_call", call_id: "past", name: "history_1", arguments: "{}" },
      { type: "function_call_output", call_id: "past", output: "past result" },
      { role: "user", content: "continue" },
    ]);
    assert.equal(payload.tools.length, 1); assert.equal(payload.tools[0].name, "tool_0");
    assert.equal(payload.tool_choice, "auto"); assert.equal(payload.parallel_tool_calls, true);
    assert.equal(payload.temperature, 0.4); assert.equal(payload.top_p, 0.8);
    return nativeResponse([{ type: "reasoning", content: [], summary: [], encrypted_content: "opaque" }, nativeText("done")]);
  } } });
  const stream = await events(await invoke(transport, {
    instructions: "instructions", temperature: 0.4, top_p: 0.8, parallel_tool_calls: true, tool_choice: "auto",
    input: [
      { role: "developer", content: [{ type: "input_text", text: "developer text" }] },
      { type: "agent_message", author: "peer", recipient: "parent", content: "update" },
      { type: "reasoning", content: [{ type: "reasoning_text", text: "historic thought" }] },
      { role: "assistant", content: "historic answer" },
      { type: "function_call", call_id: "past", name: "removed", arguments: "{}" },
      { type: "function_call_output", call_id: "past", output: "past result" },
      { type: "additional_tools", tools: [{ type: "function", name: "current" }] },
      { role: "user", content: "continue" },
    ],
  }));
  assert.equal(stream.at(-1).response.output.length, 1);
  assert.equal(stream.at(-1).response.output[0].content[0].text, "done");
  assert.equal(JSON.stringify(stream).includes("opaque"), false);
});


test("Cloudflare REST rejects malformed or ambiguous credential configuration before dispatch", () => {
  const base = { ...bindingOptions, accountId: "a".repeat(32), apiKey: secret };
  for (const extra of [{accountId:"../other"}, {accountId:"https://untrusted.invalid"}, {accountId:""},
    {apiKey:undefined}, {apiKey:""}, {apiKey:"bad\r\nheader"}, {ai:{run(){}}}, {model:models[0]}, {fetch:42}]) {
    assert.throws(() => createGatewayResponses({...base,...extra}), /Gateway Responses/);
  }
});
for (const status of [302, 429, 503]) test(`Cloudflare REST sanitizes HTTP ${status} without retry or binding fallback`, async () => {
  const observed=[]; let calls=0;
  const transport=createGatewayResponses({...bindingOptions,accountId:"a".repeat(32),apiKey:secret,
    onRequest:()=>({headers(status){observed.push(status);},finish(outcome){observed.push(outcome);}}),
    fetch:async()=>{calls++;return new Response("private provider error "+secret,{status});}});
  await assert.rejects(()=>invoke(transport,{input:"fixture"}), error => !String(error).includes(secret) && !String(error).includes("private"));
  assert.equal(calls,1); assert.deepEqual(observed,[status,"http_error"]);
});
test("Cloudflare REST validates native Responses bodies and propagates caller cancellation", async () => {
  const observed=[];
  const transport=createGatewayResponses({...bindingOptions,accountId:"a".repeat(32),apiKey:secret,
    onRequest:()=>({headers(){},finish(outcome){observed.push(outcome);}}),
    fetch:async()=>Response.json({object:"response",status:"completed",output:[{type:"web_search_call"}]})});
  await assert.rejects(()=>invoke(transport,{input:"fixture"}));assert.deepEqual(observed,["protocol_error"]);
  const controller=new AbortController();let sent;
  const cancelled=createGatewayResponses({...bindingOptions,accountId:"a".repeat(32),apiKey:secret,
    fetch:async(_url,init)=>{sent=init.signal;controller.abort(new DOMException("Stopped","AbortError"));throw controller.signal.reason;}});
  await assert.rejects(()=>invoke(cancelled,{input:"fixture"},controller.signal),{name:"AbortError"});assert.equal(sent,controller.signal);
});

for (const provider of ["openrouter", "vercel"]) for (const model of ["kimi-k3", "mimo-v2.6-pro"]) {
  test(`${provider}/${model} preserves vision, namespaced tools and reasoning across a tool round trip`, async () => {
    const requests = [];
    const details = [{ type: "reasoning.text", text: "provider trace", signature: "fixture", index: 0 }];
    const image = "data:image/png;base64,aW1hZ2U=";
    const transport = createGatewayResponses({ ...options, provider, model, reasoningEffort: "low", fetch: async (_url, init) => {
      const body = JSON.parse(init.body); requests.push(body);
      assert.equal(body.model, model === "kimi-k3" ? "moonshotai/kimi-k3" : "xiaomi/mimo-v2.6-pro");
      assert.deepEqual(body.reasoning, { effort: "low" });
      if (requests.length === 1) {
        assert.equal(body.messages[0].content[1].image_url.url, image);
        return completion({ reasoning_content: "reasoning", reasoning_details: details,
          tool_calls: [{ id: "call", function: { name: "tool_0", arguments: '{"input":"text(42)"}' } }] }, "tool_calls");
      }
      assert.deepEqual(body.messages[1].reasoning_details, details);
      assert.equal(body.messages[1].reasoning_content, "reasoning");
      assert.equal(body.messages[1].tool_calls[0].id, "call");
      assert.equal(body.messages[2].role, "tool");
      assert.equal(body.messages[2].content, "tool result");
      assert.equal(body.messages[3].content[1].image_url.url, image);
      return completion({ content: "observed" });
    }});
    const tools = [{ type: "namespace", name: "functions", tools: [{ type: "custom", name: "exec" }] }];
    const input = [{ role: "user", content: [{ type: "input_text", text: "Observe" }, { type: "input_image", image_url: image }] }];
    const first = (await events(await invoke(transport, { model, tools, input }))).at(-1).response;
    assert.equal(first.output.at(-1).namespace, "functions");
    const second = (await events(await invoke(transport, { model, tools, input: [...input, ...first.output,
      { type: "custom_tool_call_output", call_id: "call", output: [{ type: "input_text", text: "tool result" }, { type: "input_image", image_url: image }] }] }))).at(-1).response;
    assert.equal(second.output[0].content[0].text, "observed");
  });
}
test("gateway-only model and unsupported effort fail before network access", () => {
  for (const model of ["kimi-k3", "mimo-v2.6-pro"]) assert.throws(() => createGatewayResponses({provider:"cloudflare",model,reasoningEffort:"low", ai:{run(){throw Error("network")}}}));
  assert.throws(() => createGatewayResponses({...options,model:"kimi-k3",reasoningEffort:"medium"}));
});

test("parallel image tool outputs stay paired before user observations", async () => {
  const image = "data:image/png;base64,aW1hZ2U=";
  const transport = createGatewayResponses({ ...options, model: "mimo-v2.6-pro", fetch: async (_url, init) => {
    const messages = JSON.parse(init.body).messages;
    assert.deepEqual(messages.map(m => m.role), ["user", "assistant", "tool", "tool", "user", "user"]);
    assert.deepEqual(messages.slice(2, 4).map(m => m.tool_call_id), ["a", "b"]);
    assert.ok(messages[4].content[0].text.includes("a"));
    assert.ok(messages[5].content[0].text.includes("b"));
    return completion({ content: "observed both" });
  }});
  await invoke(transport, { tools: [{ type: "function", name: "screenshot" }], input: [
    { role: "user", content: "Compare" },
    ...["a", "b"].map(call_id => ({ type: "function_call", name: "screenshot", call_id, arguments: "{}" })),
    ...["a", "b"].map(call_id => ({ type: "function_call_output", call_id, output: [{ type: "input_image", image_url: image }] })),
  ] });
});

test("Cloudflare frontier binding keeps screenshot history in Responses format", async () => {
  const image = "https://example.invalid/screenshot.png";
  const transport = createGatewayResponses({ provider: "cloudflare", model: "gpt-6-astra", reasoningEffort: "low",
    ai: { async run(_model, body) {
      assert.deepEqual(body.input[0].content, [{ type: "input_text", text: "Observe" }, { type: "input_image", image_url: image, detail: "high" }]);
      return { object: "response", status: "completed", output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "observed" }] }] };
    }} });
  await invoke(transport, { input: [{ role: "user", content: [{ type: "input_text", text: "Observe" }, { type: "input_image", image_url: image, detail: "original" }] }] });
});

for (const incorrect of [false, true]) test(`MiMo OpenRouter emulates forced tool choice and rejects a missing call=${incorrect}`, async () => {
  const transport = createGatewayResponses({ ...options, model: "mimo-v2.6-pro", fetch: async (_url, init) => {
    const body = JSON.parse(init.body);
    assert.equal(body.tool_choice, "auto"); assert.equal(body.tools.length, 1);
    assert.equal(body.tools[0].function.name, "tool_1");
    return incorrect ? completion({ content: "ignored" }) : completion({ tool_calls: [{ id: "forced", function: { name: "tool_1", arguments: "{}" } }] }, "tool_calls");
  }});
  const pending = invoke(transport, { tools: [{ type: "function", name: "other" }, { type: "function", name: "chosen" }], tool_choice: { type: "function", name: "chosen" }, input: "Use chosen" });
  if (incorrect) await assert.rejects(pending);
  else assert.equal((await events(await pending)).at(-1).response.output[0].name, "chosen");
});

test("buffered gateway reasoning details retain visible text without duplicating legacy mirrors", async () => {
  for (const mirrored of [false, true]) {
    const transport = createGatewayResponses({ provider: "openrouter", model: "kimi-k3", reasoningEffort: "low", apiKey: "synthetic-key",
      fetch: async () => Response.json({ choices: [{ message: { content: "Answer", ...(mirrored ? { reasoning: "Inspect fixture" } : {}),
        reasoning_details: [{ type: "reasoning.text", text: "Inspect " }, { type: "reasoning.summary", summary: "fixture" },
          { type: "reasoning.encrypted", data: "synthetic-opaque" }] }, finish_reason: "stop" }] }) });
    const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
      authorization: "host_managed", body: JSON.stringify({ input: "Inspect" }) });
    const events = (await response.text()).split("\n\n").filter(Boolean).map(frame => JSON.parse(frame.split("\ndata: ")[1]));
    assert.deepEqual(events.filter(e => e.type === "response.reasoning_text.delta").map(e => e.delta), ["Inspect fixture"]);
  }
});
