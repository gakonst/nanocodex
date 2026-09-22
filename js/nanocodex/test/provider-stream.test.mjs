import test from "node:test";
import assert from "node:assert/strict";
import { createGatewayResponses } from "../cloudflare/gateway-responses.mjs";
import { createWorkersAiResponses } from "../cloudflare/workers-ai-responses.mjs";
const encoder = new TextEncoder();
const wire = value => `data: ${typeof value === "string" ? value : JSON.stringify(value)}\r\n\r\n`;
const chunk = (delta, finish_reason = null) => ({ choices: [{ index: 0, delta, finish_reason }] });
function feed() {
  let controller, cancelled = 0;
  const body = new ReadableStream({ start(value) { controller = value; }, cancel() { cancelled++; } });
  return { body, send(value) { controller.enqueue(encoder.encode(wire(value))); }, raw(value) { controller.enqueue(value); },
    close() { controller.close(); }, get cancelled() { return cancelled; } };
}
function setup(provider, upstream, signal) {
  const observed = [], requests = [];
  const options = { provider, model: "gpt-5.6-sol", reasoningEffort: "high", apiKey: "synthetic-secret",
    ...(provider === "cloudflare" ? { accountId: "a".repeat(32) } : {}),
    fetch: async (_url, init) => { requests.push(JSON.parse(init.body)); return new Response(upstream.body, { headers: { "content-type": "text/event-stream" } }); },
    onRequest: () => ({ headers(status) { observed.push(status); }, firstToken() { observed.push("first"); }, finish(outcome) { observed.push(outcome); } }) };
  const transport = createGatewayResponses(options);
  return { observed, requests, invoke: (body = {}) => transport.createResponse(`${transport.apiBaseUrl}/responses`, "synthetic", {
    authorization: "host_managed", body: JSON.stringify({ input: "hello", stream: true, ...body }), signal,
  }) };
}
async function next(reader) {
  const value = await reader.read();
  if (value.done) return null;
  return JSON.parse(new TextDecoder().decode(value.value).split("\ndata: ")[1]);
}
async function until(reader, type) {
  while (true) { const event = await next(reader); if (!event || event.type === type) return event; }
}
async function all(response) {
  const text = await response.text();
  return text.split("\n\n").filter(Boolean).map(frame => JSON.parse(frame.split("\ndata: ")[1]));
}
const nativeItem = { type: "message", id: "upstream-msg", role: "assistant", status: "in_progress", content: [] };
const nativeFinal = (text = "hello") => ({ type: "response.completed", response: { id: "upstream-resp", object: "response", status: "completed",
  output: [{ ...nativeItem, status: "completed", content: [{ type: "output_text", text, annotations: [] }] }],
  usage: { input_tokens: 2, output_tokens: 1, total_tokens: 3 } } });
for (const provider of ["openrouter", "vercel", "cloudflare"]) test(`${provider}: first text is delivered before gated completion and telemetry finishes afterward`, async () => {
  const upstream = feed(), fixture = setup(provider, upstream);
  const response = await fixture.invoke();
  assert.equal(fixture.requests[0].stream, true);
  assert.equal(response.headers.get("x-nanocodex-inference-buffering"), "streaming");
  assert.deepEqual(fixture.observed, [200]);
  const reader = response.body.getReader();
  assert.equal((await next(reader)).type, "response.created");
  if (provider === "cloudflare") {
    upstream.send({ type: "response.created", response: { status: "in_progress" } });
    upstream.send({ type: "response.output_item.added", output_index: 0, item: nativeItem });
    upstream.send({ type: "response.output_text.delta", output_index: 0, item_id: nativeItem.id, content_index: 0, delta: "hello" });
  } else { upstream.send(chunk({ role: "assistant" })); upstream.send(chunk({ content: "hello" })); }
  assert.equal((await until(reader, "response.output_text.delta")).delta, "hello");
  assert.deepEqual(fixture.observed, [200, "first"]);
  if (provider === "cloudflare") upstream.send(nativeFinal());
  else { upstream.send(chunk({}, "stop")); upstream.send({ choices: [], usage: { prompt_tokens: 2, completion_tokens: 1, total_tokens: 3 } }); upstream.send("[DONE]"); }
  const terminal = await until(reader, "response.completed");
  assert.equal(terminal.response.output[0].content[0].text, "hello");
  assert.equal(terminal.response.usage.total_tokens, 3);
  assert.equal(await next(reader), null);
  assert.deepEqual(fixture.observed, [200, "first", "success"]);
  assert.equal(upstream.cancelled, 1);
});

test("Chat fragmented namespaced custom, function and search tools validate and round trip", async () => {
  const upstream = feed(), fixture = setup("openrouter", upstream);
  const response = await fixture.invoke({ tools: [{ type: "namespace", name: "files", tools: [{ type: "custom", name: "edit" }] },
    { type: "function", name: "read" }, { type: "tool_search", execution: "client" }] });
  const pending = all(response);
  for (const [index, args] of ['{"input":"text(42)\\n"}', '{"path":"a"}', '{"query":"x"}'].entries()) {
    upstream.send(chunk({ tool_calls: [{ index, id: `call-${index}`, type: "function", function: { name: "tool_", arguments: "" } }] }));
    upstream.send(chunk({ tool_calls: [{ index, function: { name: String(index), arguments: args.slice(0, 4) } }] }));
    upstream.send(chunk({ tool_calls: [{ index, function: { arguments: args.slice(4) } }] }));
  }
  upstream.send(chunk({}, "tool_calls")); upstream.send("[DONE]");
  const events = await pending, output = events.at(-1).response.output;
  assert.deepEqual(output.map(item => item.type), ["custom_tool_call", "function_call", "tool_search_call"]);
  assert.equal(output[0].namespace, "files"); assert.equal(output[0].input, "text(42)\n");
  assert.equal(output[1].arguments, '{"path":"a"}'); assert.deepEqual(output[2].arguments, { query: "x" });
  assert.deepEqual(events.map(event => event.sequence_number), events.map((_, index) => index));
  assert.deepEqual(fixture.observed, [200, "first", "success"]);
});

test("native Responses tools map aliases and reject mismatched streamed arguments", async () => {
  for (const mismatch of [false, true]) {
    const upstream = feed(), fixture = setup("cloudflare", upstream);
    const response = await fixture.invoke({ tools: [{ type: "function", name: "read" }] });
    const pending = all(response);
    const item = { id: "tool-native", type: "function_call", call_id: "call-native", name: "tool_0", arguments: "", status: "in_progress" };
    upstream.send({ type: "response.output_item.added", output_index: 0, item });
    upstream.send({ type: "response.function_call_arguments.delta", output_index: 0, item_id: item.id, delta: '{"x":1}' });
    upstream.send({ type: "response.completed", response: { object: "response", status: "completed", output: [{ ...item, status: "completed", arguments: mismatch ? '{"x":2}' : '{"x":1}' }] } });
    if (mismatch) await assert.rejects(pending, /invalid provider stream/);
    else assert.equal((await pending).at(-1).response.output[0].name, "read");
  }
});

for (const scenario of ["early-close", "missing-done", "malformed", "provider-error", "unknown-tool", "duplicate-tool", "incomplete-tool", "parallel-tool", "truncated-frame", "invalid-utf8"]) {
  test(`stream fails closed and redacts ${scenario}`, async () => {
    const upstream = feed(), fixture = setup("openrouter", upstream);
    const response = await fixture.invoke({ tools: [{ type: "function", name: "read" }], parallel_tool_calls: false });
    const pending = all(response);
    if (scenario === "malformed") upstream.send("synthetic-secret");
    else if (scenario === "provider-error") upstream.send({ error: { message: "synthetic-secret" } });
    else if (scenario === "truncated-frame") upstream.raw(encoder.encode('data: {"synthetic-secret":'));
    else if (scenario === "invalid-utf8") upstream.raw(new Uint8Array([255, 10, 10]));
    else if (["unknown-tool", "duplicate-tool", "incomplete-tool", "parallel-tool"].includes(scenario)) {
      const count = ["duplicate-tool", "parallel-tool"].includes(scenario) ? 2 : 1;
      for (let index = 0; index < count; index++) upstream.send(chunk({ tool_calls: [{ index,
        id: scenario === "duplicate-tool" ? "same" : `call-${index}`, type: "function",
        function: { name: scenario === "unknown-tool" ? "synthetic-secret" : "tool_0", arguments: '{"x":1}' } }] }));
      upstream.send(chunk({}, scenario === "incomplete-tool" ? "length" : "tool_calls")); upstream.send("[DONE]");
    } else if (scenario === "missing-done") upstream.send(chunk({ content: "ok" }, "stop"));
    upstream.close();
    await assert.rejects(pending, error => /invalid provider stream/.test(error.message) && !error.message.includes("synthetic-secret"));
    assert.equal(fixture.observed.at(-1), "protocol_error");
    assert.equal(fixture.observed.filter(value => value === "protocol_error").length, 1);
  });
}

test("abort and downstream cancellation release upstream and finish once", async () => {
  for (const kind of ["abort", "timeout", "reader"]) {
    const upstream = feed(), controller = new AbortController(), fixture = setup("vercel", upstream, controller.signal);
    const response = await fixture.invoke(), reader = response.body.getReader();
    await next(reader);
    const pending = reader.read();
    if (kind === "reader") { await reader.cancel(); assert.equal((await pending).done, true); }
    else {
      controller.abort(new DOMException("synthetic-secret", kind === "timeout" ? "TimeoutError" : "AbortError"));
      await assert.rejects(pending, error => error.name === (kind === "timeout" ? "TimeoutError" : "AbortError") && !error.message.includes("synthetic-secret"));
    }
    assert.equal(upstream.cancelled, 1);
    assert.deepEqual(fixture.observed, [200, kind === "timeout" ? "timeout" : "cancelled"]);
  }
});

test("Workers AI ReadableStream handles fragmented UTF-8 and CRLF SSE", async () => {
  const upstream = feed();
  const transport = createWorkersAiResponses({ async run(_model, input) { assert.equal(input.stream, true); return upstream.body; } });
  const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", { authorization: "host_managed", body: JSON.stringify({ input: "hi", stream: true }) });
  const reader = response.body.getReader();
  await next(reader);
  const bytes = encoder.encode(`: keepalive\r\n\r\n${wire(chunk({ content: "héllo 🌍" }))}`);
  for (const byte of bytes) upstream.raw(new Uint8Array([byte]));
  assert.equal((await until(reader, "response.output_text.delta")).delta, "héllo 🌍");
  upstream.send(chunk({}, "stop")); upstream.send("[DONE]");
  assert.equal((await until(reader, "response.completed")).response.output[0].content[0].text, "héllo 🌍");
});

test("upstream body read errors are redacted network failures", async () => {
  let controller;
  const upstream = { body: new ReadableStream({ start(value) { controller = value; } }) };
  const fixture = setup("vercel", upstream), response = await fixture.invoke();
  const pending = all(response);
  controller.error(new Error("synthetic-secret"));
  await assert.rejects(pending, error => error.message === "Responses: provider stream read failed");
  assert.deepEqual(fixture.observed, [200, "network_error"]);
});

test("incomplete text is terminal incomplete and reasoning uses stable paired items", async () => {
  const upstream = feed(), fixture = setup("openrouter", upstream), response = await fixture.invoke();
  const reader = response.body.getReader();
  await next(reader);
  upstream.send(chunk({ reasoning_content: "think" }));
  const first = await until(reader, "response.reasoning_text.delta");
  assert.equal(first.delta, "think"); assert.deepEqual(fixture.observed, [200]);
  upstream.send(chunk({ content: "partial" }));
  upstream.send(chunk({}, "length")); upstream.send("[DONE]");
  const terminal = await until(reader, "response.incomplete");
  assert.equal(terminal.response.output[0].id, first.item_id);
  assert.equal(terminal.response.end_turn, false);
  assert.deepEqual(terminal.response.incomplete_details, { reason: "max_output_tokens" });
});

test("streaming retains pre-dispatch full-history and tool validation", async () => {
  for (const input of [ { previous_response_id: "opaque" }, { input: [{ type: "function_call", call_id: "c", name: "read", arguments: "{}" }] },
    { input: [{ role: "user", content: [{ type: "input_image", image_url: "synthetic-secret" }] }] }, { tool_choice: { type: "function", name: "undeclared" } } ]) {
    const upstream = feed(), fixture = setup("cloudflare", upstream);
    await assert.rejects(() => fixture.invoke(input), /request failed/);
    assert.equal(fixture.requests.length, 0); assert.deepEqual(fixture.observed, []);
  }
});

test("Cloudflare binding streams native Responses before completion", async () => {
  const upstream = feed(), observed = [];
  const transport = createGatewayResponses({ provider: "cloudflare", model: "gpt-5.6-sol", reasoningEffort: "high",
    ai: { async run(_model, input) { assert.equal(input.stream, true); return upstream.body; } },
    onRequest: () => ({ headers() { assert.fail(); }, firstToken() { observed.push("first"); }, finish(outcome) { observed.push(outcome); } }) });
  const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
    authorization: "host_managed", body: JSON.stringify({ stream: true, input: "hi" }) });
  const reader = response.body.getReader(); await next(reader);
  upstream.send({ type: "response.output_item.added", output_index: 0, item: nativeItem });
  upstream.send({ type: "response.output_text.delta", output_index: 0, item_id: nativeItem.id, delta: "hello" });
  assert.equal((await until(reader, "response.output_text.delta")).delta, "hello");
  assert.deepEqual(observed, ["first"]);
  upstream.send(nativeFinal());
  await until(reader, "response.completed"); await next(reader);
  assert.deepEqual(observed, ["first", "success"]);
});

test("late binding streams are cancelled after dispatch was aborted", async () => {
  for (const gateway of [false, true]) {
    let resolve, begin;
    const ready = new Promise(value => { begin = value; });
    const ai = { run() { begin(); return new Promise(value => { resolve = value; }); } };
    const transport = gateway ? createGatewayResponses({ provider: "cloudflare", model: "gpt-5.6-sol", reasoningEffort: "high", ai }) : createWorkersAiResponses(ai);
    const controller = new AbortController();
    const pending = transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
      authorization: "host_managed", body: JSON.stringify({ input: "hi", stream: true }), signal: controller.signal });
    await ready; controller.abort();
    await assert.rejects(pending, { name: "AbortError" });
    const upstream = feed(); resolve(upstream.body);
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(upstream.cancelled, 1);
  }
});

for (const scenario of ["early-close", "failed", "mismatched-final", "unknown-item", "event-mismatch"]) test(`native Responses rejects ${scenario}`, async () => {
  const upstream = feed(), fixture = setup("cloudflare", upstream), response = await fixture.invoke();
  const pending = all(response);
  upstream.send({ type: "response.output_item.added", output_index: 0, item: nativeItem });
  if (scenario === "failed") upstream.send({ type: "response.failed", response: { error: { message: "synthetic-secret" } } });
  else if (scenario === "event-mismatch") upstream.raw(encoder.encode(`event: response.completed\ndata: ${JSON.stringify({ type: "response.in_progress" })}\n\n`));
  else {
    upstream.send({ type: "response.output_text.delta", output_index: 0, item_id: scenario === "unknown-item" ? "unknown" : nativeItem.id, delta: "hello" });
    if (scenario === "mismatched-final") upstream.send(nativeFinal("different"));
  }
  upstream.close();
  await assert.rejects(pending, error => error.message === "Responses: invalid provider stream");
  assert.equal(fixture.observed.at(-1), "protocol_error");
});

test("completed binding objects honestly report buffered fallback for stream requests", async () => {
  for (const gateway of [false, true]) {
    const observed = [];
    const ai = { async run(_model, input) {
      assert.equal(input.stream, true);
      return gateway ? nativeFinal().response : { choices: [{ message: { content: "hello" }, finish_reason: "stop" }] };
    } };
    const transport = gateway ? createGatewayResponses({ provider: "cloudflare", model: "gpt-5.6-sol", reasoningEffort: "high", ai,
      onRequest: () => ({ headers() {}, firstToken() { observed.push("first"); }, finish(outcome) { observed.push(outcome); } }) }) : createWorkersAiResponses(ai);
    const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
      authorization: "host_managed", body: JSON.stringify({ input: "hi", stream: true }) });
    assert.equal(response.headers.get("x-nanocodex-inference-buffering"), "buffered");
    assert.equal((await all(response)).at(-1).response.output[0].content[0].text, "hello");
    if (gateway) assert.deepEqual(observed, ["success"]);
  }
});

test("explicit buffered requests are labeled and HTTP streaming never falls back to JSON", async () => {
  const transport = createWorkersAiResponses({ async run() { return { choices: [{ message: { content: "hello" }, finish_reason: "stop" }] }; } });
  const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
    authorization: "host_managed", body: JSON.stringify({ input: "hi" }) });
  assert.equal(response.headers.get("x-nanocodex-inference-buffering"), "buffered");
  for (const provider of ["openrouter", "vercel", "cloudflare"]) {
    const http = createGatewayResponses({ provider, model: "gpt-5.6-sol", reasoningEffort: "high", apiKey: "synthetic",
      ...(provider === "cloudflare" ? { accountId: "a".repeat(32) } : {}), fetch: async () => Response.json(nativeFinal().response) });
    await assert.rejects(() => http.createResponse(`${http.apiBaseUrl}/responses`, "fixture", {
      authorization: "host_managed", body: JSON.stringify({ input: "hi", stream: true }) }), /request failed/);
  }
});

for (const scenario of ["unterminated-frame", "aggregate-text", "aggregate-tools", "excessive-index"]) {
  test(`stream memory is bounded: ${scenario}`, async () => {
    const upstream = feed(), fixture = setup("openrouter", upstream), response = await fixture.invoke({ tools: [{ type: "function", name: "read" }] });
    const pending = all(response);
    const block = "x".repeat(1024 * 1024);
    if (scenario === "unterminated-frame") upstream.raw(encoder.encode(`data: ${block.repeat(4)}`));
    else if (scenario === "aggregate-text") for (let index = 0; index < 9; index++) upstream.send(chunk({ content: block }));
    else if (scenario === "aggregate-tools") for (let index = 0; index < 5; index++) upstream.send(chunk({ tool_calls: [{ index: 0, function: { arguments: block } }] }));
    else upstream.send(chunk({ tool_calls: [{ index: 1024, function: { name: "tool_0" } }] }));
    await assert.rejects(pending, { message: "Responses: invalid provider stream" });
    assert.equal(fixture.observed.at(-1), "protocol_error");
    assert.equal(upstream.cancelled, 1);
  });
}

test("tool-only first-token telemetry waits for validated public tool output", async () => {
  const upstream = feed(), fixture = setup("openrouter", upstream), response = await fixture.invoke({ tools: [{ type: "function", name: "read" }] });
  const pending = all(response);
  upstream.send(chunk({ tool_calls: [{ index: 0, id: "call-1", type: "function", function: { name: "tool_0", arguments: "{}" } }] }));
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(fixture.observed, [200]);
  upstream.send(chunk({}, "tool_calls")); upstream.send("[DONE]");
  const events = await pending;
  assert.equal(events.at(-1).response.output[0].name, "read");
  assert.deepEqual(fixture.observed, [200, "first", "success"]);
});

test("reasoning-only output never records public first-token telemetry", async () => {
  const upstream = feed(), fixture = setup("vercel", upstream), response = await fixture.invoke();
  const pending = all(response);
  upstream.send(chunk({ reasoning_content: "thinking" })); upstream.send(chunk({}, "stop")); upstream.send("[DONE]");
  await pending;
  assert.deepEqual(fixture.observed, [200, "success"]);
});

test("native Responses reject changes to declared tool identity", async () => {
  for (const field of ["call_id", "name"]) {
    const upstream = feed(), fixture = setup("cloudflare", upstream), response = await fixture.invoke({ tools: [{ type: "function", name: "read" }, { type: "function", name: "write" }] });
    const pending = all(response);
    const item = { id: "native-tool", type: "function_call", call_id: "call-1", name: "tool_0", arguments: "", status: "in_progress" };
    upstream.send({ type: "response.output_item.added", output_index: 0, item });
    upstream.send({ type: "response.function_call_arguments.delta", output_index: 0, item_id: item.id, delta: "{}" });
    upstream.send({ type: "response.completed", response: { object: "response", status: "completed", output: [{ ...item, status: "completed", arguments: "{}", [field]: field === "name" ? "tool_1" : "call-2" }] } });
    await assert.rejects(pending, { message: "Responses: invalid provider stream" });
    assert.deepEqual(fixture.observed, [200, "protocol_error"]);
  }
});

test("native Responses retain multiple text parts with portable newline boundaries", async () => {
  const upstream = feed(), fixture = setup("cloudflare", upstream), response = await fixture.invoke();
  const pending = all(response);
  upstream.send({ type: "response.output_item.added", output_index: 0, item: nativeItem });
  for (const [content_index, delta] of ["hello", "world"].entries()) upstream.send({ type: "response.output_text.delta", output_index: 0, item_id: nativeItem.id, content_index, delta });
  const terminal = nativeFinal();
  terminal.response.output[0].content = ["hello", "world"].map(text => ({ type: "output_text", text, annotations: [] }));
  upstream.send(terminal);
  const events = await pending;
  assert.equal(events.filter(event => event.type === "response.output_text.delta").map(event => event.delta).join(""), "hello\nworld");
  assert.equal(events.at(-1).response.output[0].content[0].text, "hello\nworld");
});

test("native terminal-only public text records first-token once and reasoning does not", async () => {
  for (const kind of ["message", "reasoning"]) {
    const upstream = feed(), fixture = setup("cloudflare", upstream), response = await fixture.invoke();
    const pending = all(response);
    const terminal = nativeFinal();
    if (kind === "reasoning") terminal.response.output = [{ id: "reasoning-1", type: "reasoning", status: "completed",
      summary: [], content: [{ type: "reasoning_text", text: "thinking" }] }];
    upstream.send(terminal);
    const events = await pending;
    assert.equal(events.at(-1).type, "response.completed");
    assert.deepEqual(fixture.observed, kind === "message" ? [200, "first", "success"] : [200, "success"]);
  }
});

for (const scenario of ["aggregate-distinct-tools", "total-wire-keepalives"]) {
  test(`stream memory is bounded: ${scenario}`, async () => {
    const upstream = feed(), fixture = setup("openrouter", upstream), response = await fixture.invoke();
    const pending = all(response);
    const block = "x".repeat(1024 * 1024);
    if (scenario === "aggregate-distinct-tools") {
      for (let index = 0; index < 9; index++) upstream.send(chunk({ tool_calls: [{ index,
        id: `call-${index}`, function: { name: "tool_0", arguments: block } }] }));
    } else {
      for (let index = 0; index < 33; index++) upstream.raw(encoder.encode(`: ${block}\n\n`));
    }
    await assert.rejects(pending, { message: "Responses: invalid provider stream" });
    assert.deepEqual(fixture.observed, [200, "protocol_error"]);
    assert.equal(upstream.cancelled, 1);
  });
}

test("terminal success and downstream cancellation do not wait for a hanging upstream cancel", { timeout: 2000 }, async () => {
  for (const terminal of [false, true]) {
    let controller, cancelled = 0;
    const upstream = { body: new ReadableStream({ start(value) { controller = value; },
      cancel() { cancelled++; return new Promise(() => {}); } }) };
    const fixture = setup("vercel", upstream), response = await fixture.invoke();
    if (terminal) {
      const pending = all(response);
      controller.enqueue(encoder.encode(wire(chunk({ content: "hello" }, "stop")) + wire("[DONE]")));
      assert.equal((await pending).at(-1).type, "response.completed");
      assert.deepEqual(fixture.observed, [200, "first", "success"]);
    } else {
      await response.body.cancel();
      assert.deepEqual(fixture.observed, [200, "cancelled"]);
    }
    assert.equal(cancelled, 1);
  }
});


test("validated custom tool calls with empty input still record public first-token telemetry", async () => {
  const upstream = feed(), fixture = setup("openrouter", upstream);
  const response = await fixture.invoke({ tools: [{ type: "custom", name: "exec" }] });
  const pending = all(response);
  upstream.send(chunk({ tool_calls: [{ index: 0, id: "call-empty", type: "function",
    function: { name: "tool_0", arguments: JSON.stringify({ input: "" }) } }] }));
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(fixture.observed, [200]);
  upstream.send(chunk({}, "tool_calls")); upstream.send("[DONE]");
  assert.equal((await pending).at(-1).response.output[0].input, "");
  assert.deepEqual(fixture.observed, [200, "first", "success"]);
});


test("OpenRouter repeated finish chunk carries usage before DONE without duplicating output", async () => {
  for (const finish of ["stop", "tool_calls"]) {
    const upstream = feed(), fixture = setup("openrouter", upstream);
    const response = await fixture.invoke({ tools: [{ type: "function", name: "read" }] });
    const pending = all(response);
    if (finish === "stop") upstream.send(chunk({ role: "assistant", content: "hello" }));
    else upstream.send(chunk({ tool_calls: [{ index: 0, id: "call-1", type: "function",
      function: { name: "tool_0", arguments: "{}" } }] }));
    upstream.send(chunk({ content: "", role: "assistant" }, finish));
    // Observed live OpenRouter shape: same empty choice/finish_reason repeated
    // on the usage trailer, followed by [DONE]. No provider text is retained.
    upstream.send({ ...chunk({ content: "", role: "assistant" }, finish), usage: {
      prompt_tokens: 18, completion_tokens: 45, total_tokens: 63,
      prompt_tokens_details: { cached_tokens: 0 }, completion_tokens_details: { reasoning_tokens: 0 },
    } });
    upstream.send("[DONE]");
    const events = await pending, terminal = events.at(-1);
    assert.equal(terminal.type, "response.completed");
    assert.equal(terminal.response.output.length, 1);
    assert.equal(terminal.response.usage.total_tokens, 63);
    assert.equal(events.filter(event => event.type === "response.output_item.done").length, 1);
    assert.deepEqual(fixture.observed, [200, "first", "success"]);
  }
});

for (const scenario of ["no-usage", "changed-finish", "new-text", "new-reasoning", "new-tool", "unknown-delta-field", "missing-DONE"]) {
  test(`Chat usage trailers reject ${scenario}`, async () => {
    const upstream = feed(), fixture = setup("openrouter", upstream), response = await fixture.invoke();
    const pending = all(response);
    upstream.send(chunk({ content: "hello" }, "stop"));
    const trailer = { ...chunk({ content: "", role: "assistant" }, "stop"), usage: { total_tokens: 2 } };
    if (scenario === "no-usage") delete trailer.usage;
    if (scenario === "changed-finish") trailer.choices[0].finish_reason = "length";
    if (scenario === "new-text") trailer.choices[0].delta.content = "unexpected";
    if (scenario === "new-reasoning") trailer.choices[0].delta.reasoning_content = "unexpected";
    if (scenario === "new-tool") trailer.choices[0].delta.tool_calls = [{ index: 0, function: { arguments: "{}" } }];
    if (scenario === "unknown-delta-field") trailer.choices[0].delta.unrecognized = "synthetic-secret";
    upstream.send(trailer);
    if (scenario !== "missing-DONE") upstream.send("[DONE]");
    upstream.close();
    await assert.rejects(pending, { message: "Responses: invalid provider stream" });
    assert.deepEqual(fixture.observed, [200, "first", "protocol_error"]);
  });
}

for (const model of ["kimi-k3", "mimo-v2.6-pro"]) test(`${model}: streamed opaque reasoning survives a real tool-history replay`, async () => {
  const upstream = feed();
  const details = [{ type: "reasoning.encrypted", data: "fixture-part-a", index: 0 }, { type: "reasoning.encrypted", data: "fixture-part-b", index: 0 }];
  let count = 0;
  const transport = createGatewayResponses({ provider: "openrouter", model, reasoningEffort: "low", apiKey: "synthetic-key",
    fetch: async (_url, init) => {
      if (++count === 1) return new Response(upstream.body, { headers: { "content-type": "text/event-stream" } });
      const messages = JSON.parse(init.body).messages;
      assert.deepEqual(messages[1].reasoning_details, details);
      assert.equal(messages[1].tool_calls[0].id, "fixture-call");
      return Response.json({ choices: [{ message: { content: "done" }, finish_reason: "stop" }] });
    } });
  const invoke = body => transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", { authorization: "host_managed", body: JSON.stringify(body) });
  const tools = [{ type: "function", name: "read", parameters: { type: "object", properties: {} } }];
  const response = await invoke({ input: "Read fixture", tools, stream: true });
  const pending = all(response);
  for (const detail of details) upstream.send(chunk({ reasoning_details: [detail] }));
  upstream.send(chunk({ tool_calls: [{ index: 0, id: "fixture-call", type: "function", function: { name: "tool_0", arguments: "{}" } }] }));
  upstream.send(chunk({}, "tool_calls")); upstream.send("[DONE]");
  const terminal = (await pending).at(-1).response;
  assert.equal(terminal.output[0].type, "reasoning");
  assert.deepEqual(terminal.output[0].content, []);
  assert.ok(terminal.output[0].encrypted_content.startsWith("nanocodex-chat-reasoning-v1:"));
  const result = await all(await invoke({ input: [{ role: "user", content: "Read fixture" }, ...terminal.output,
    { type: "function_call_output", call_id: "fixture-call", output: "fixture value" }], tools }));
  assert.equal(result.at(-1).response.output[0].content[0].text, "done");
});

async function bindingFixture(upstream) {
  const transport = createWorkersAiResponses({ async run() { return upstream.body; } });
  return transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
    authorization: "host_managed", body: JSON.stringify({ input: "Read the synthetic nonce", stream: true,
      tools: [{ type: "function", name: "read_nonce" }], parallel_tool_calls: false }),
  });
}

test("Workers AI live GLM tool shape accepts null continuation fields and aggregate usage envelope", async () => {
  const upstream = feed(), pending = all(await bindingFixture(upstream));
  upstream.send({ ...chunk({ content: "", reasoning_content: null, role: "assistant" }), usage: { prompt_tokens: 162, completion_tokens: 0 } });
  upstream.send(chunk({ content: null, reasoning_content: null, role: null, tool_calls: [{ index: 0, id: "fixture-call", type: "function", function: { name: "tool_0", arguments: "" } }] }));
  upstream.send(chunk({ content: null, reasoning_content: null, role: null, tool_calls: [{ index: 0, id: null, type: "function", function: { name: null, arguments: "{}" } }] }));
  upstream.send(chunk({ reasoning_content: null }, "tool_calls"));
  upstream.send({ choices: [], usage: { prompt_tokens: 0, completion_tokens: 1, total_tokens: 1 } });
  upstream.send({ response: "", usage: { prompt_tokens: 162, completion_tokens: 7, total_tokens: 169, prompt_tokens_details: { cached_tokens: 0 }, neurons: 23 } });
  upstream.send("[DONE]");
  const events = await pending, result = events.at(-1).response;
  assert.equal(events.at(-1).type, "response.completed");
  assert.equal(result.output[0].type, "function_call");
  assert.equal(result.output[0].name, "read_nonce");
  assert.equal(result.output[0].arguments, "{}");
  assert.equal(result.usage.input_tokens, 162);
  assert.equal(result.usage.output_tokens, 7);
  assert.equal(result.usage.total_tokens, 169);
});

test("Workers AI text arrives before its final usage envelope", async () => {
  const upstream = feed(), reader = (await bindingFixture(upstream)).body.getReader();
  await next(reader);
  upstream.send(chunk({ content: "hello", role: null }));
  assert.equal((await until(reader, "response.output_text.delta")).delta, "hello");
  upstream.send(chunk({ content: null, role: null }, "stop"));
  upstream.send({ response: "", usage: { prompt_tokens: 3, completion_tokens: 1, total_tokens: 4 } });
  upstream.send("[DONE]");
  assert.equal((await until(reader, "response.completed")).response.output[0].content[0].text, "hello");
});

for (const scenario of ["before-finish", "nonempty-response", "missing-usage", "extra-fields", "duplicate-trailer", "output-after-trailer", "missing-DONE", "gateway-envelope"]) {
  test(`Workers AI usage envelope rejects ${scenario}`, async () => {
    const upstream = feed();
    const pending = all(scenario === "gateway-envelope" ? await setup("vercel", upstream).invoke() : await bindingFixture(upstream));
    upstream.send(chunk({ content: "hello" }, scenario === "before-finish" ? null : "stop"));
    const trailer = { response: "", usage: { prompt_tokens: 3, completion_tokens: 1, total_tokens: 4 } };
    if (scenario === "nonempty-response") trailer.response = "synthetic-secret";
    if (scenario === "missing-usage") delete trailer.usage;
    if (scenario === "extra-fields") trailer.error = "synthetic-secret";
    upstream.send(trailer);
    if (scenario === "duplicate-trailer") upstream.send(trailer);
    if (scenario === "output-after-trailer") upstream.send(chunk({ content: "unexpected" }));
    if (scenario !== "missing-DONE") upstream.send("[DONE]");
    upstream.close();
    await assert.rejects(pending, { message: "Responses: invalid provider stream" });
  });
}

for (const model of ["kimi-k3", "mimo-v2.6-pro", "@cf/zai-org/glm-5.3"]) {
  test(`${model}: visible reasoning details and text cross the host HTTP bridge before gated completion`, { timeout: 2_000 }, async () => {
    const { createResponsesHttp } = await import("../runtime/responses-http.mjs");
    const upstream = feed(), requests = [];
    const transport = createGatewayResponses({ provider: "openrouter", model, reasoningEffort: "low", apiKey: "synthetic-key",
      fetch: async (_url, init) => {
        requests.push(JSON.parse(init.body));
        return new Response(upstream.body, { headers: { "content-type": "text/event-stream" } });
      } });
    const http = createResponsesHttp((_endpoint, _key, id, _metadata, body, signal) => transport.createResponse(
      `${transport.apiBaseUrl}/responses`, id, { authorization: "host_managed", body, signal }));
    const handle = http.httpOpen("fixture", "fixture", "fixture", {}, JSON.stringify({ input: "Inspect fixture", stream: true }));
    const event = async () => JSON.parse(new TextDecoder().decode(await http.httpNext(handle)).split("\ndata: ")[1]);
    const readUntil = async type => { for (;;) { const value = await event(); if (value.type === type) return value; } };
    try {
      await http.httpReady(handle);
      assert.equal(requests[0].stream, true);
      assert.equal((await event()).type, "response.created");
      // No finish chunk exists until both live reads below have resolved. A
      // buffered implementation times out instead of passing on terminal output.
      upstream.send(chunk({ reasoning: "", reasoning_details: [{ type: "reasoning.text", text: "Inspect ", index: 0 }] }));
      assert.equal((await readUntil("response.reasoning_text.delta")).delta, "Inspect ");
      await new Promise(resolve => setTimeout(resolve, 15));
      upstream.send(chunk({ reasoning_details: [{ type: "reasoning.summary", summary: "fixture", index: 1 },
        { type: "reasoning.encrypted", data: "synthetic-opaque", index: 2 }] }));
      assert.equal((await readUntil("response.reasoning_text.delta")).delta, "fixture");
      upstream.send(chunk({ content: "Result" }));
      assert.equal((await readUntil("response.output_text.delta")).delta, "Result");
      assert.equal(upstream.cancelled, 0);
      upstream.send(chunk({}, "stop")); upstream.send("[DONE]");
      const terminal = await readUntil("response.completed");
      assert.equal(terminal.response.output[0].content[0].text, "Inspect fixture");
      assert.ok(terminal.response.output[0].encrypted_content.startsWith("nanocodex-chat-reasoning-v1:"));
      assert.equal(terminal.response.output[1].content[0].text, "Result");
      assert.equal(await http.httpNext(handle), null);
    } finally { http.dispose(); }
  });
}

test("mirrored reasoning details produce one live delta and preserve their replay payload", async () => {
  const upstream = feed(), fixture = setup("openrouter", upstream), pending = all(await fixture.invoke());
  upstream.send(chunk({ reasoning: "Inspect", reasoning_details: [{ type: "reasoning.text", text: "Inspect", signature: "fixture-signature" }] }));
  upstream.send(chunk({}, "stop")); upstream.send("[DONE]");
  const events = await pending;
  assert.deepEqual(events.filter(e => e.type === "response.reasoning_text.delta").map(e => e.delta), ["Inspect"]);
  assert.equal(events.at(-1).response.output[0].content[0].text, "Inspect");
});

for (const detail of [{ type: "reasoning.text", text: { private: "fixture" } }, { type: "reasoning.summary", summary: 42 }]) {
  test(`malformed visible ${detail.type} fails sanitized`, async () => {
    const upstream = feed(), fixture = setup("openrouter", upstream), pending = all(await fixture.invoke());
    upstream.send(chunk({ reasoning_details: [detail] }));
    upstream.send(chunk({}, "stop")); upstream.send("[DONE]");
    await assert.rejects(pending, { message: "Responses: invalid provider stream" });
  });
}
