import assert from "node:assert/strict";
import { test } from "node:test";
import { webcrypto } from "node:crypto";
import { createWorkersAiResponses } from "../cloudflare/workers-ai-responses.mjs";
if (!globalThis.crypto) globalThis.crypto = webcrypto;

const completion = (message, finish_reason = "stop") => ({ choices: [{ message, finish_reason }],
  usage: { prompt_tokens: 10, completion_tokens: 4, total_tokens: 14 } });
function fixture(run) {
  const transport = createWorkersAiResponses({ run });
  return (body, signal = new AbortController().signal) => transport.createResponse(`${transport.apiBaseUrl}/responses`, "test-session", {
    authorization: "host_managed", body: JSON.stringify(body), signal,
  });
}
async function events(response) {
  assert.equal(response.headers.get("content-type"), "text/event-stream");
  return (await response.text()).split("\n\n").filter(Boolean).map(frame => JSON.parse(frame.split("\ndata: ")[1]));
}
const custom = { type: "custom", name: "exec", description: "Run code", format: { syntax: "lark", definition: "start: CODE" } };
const namespace = { type: "namespace", name: "files", tools: [{ type: "function", name: "read", parameters: { type: "object" } }] };
const search = { type: "tool_search", execution: "client", parameters: { type: "object" } };

test("translates full history, declarations, namespaced calls, and custom code without loss", async () => {
  const code = 'text(`a\\nb`);\ntext("é");';
  const invoke = fixture(async (model, input) => {
    assert.equal(model, "@cf/zai-org/glm-5.3");
    assert.equal(input.stream, false);
    assert.equal(input.max_completion_tokens, 123);
    assert.equal(input.messages[0].content, "Follow instructions");
    assert.equal(input.messages[1].role, "system");
    const calls = input.messages.find(message => message.tool_calls).tool_calls;
    assert.equal(JSON.parse(calls[0].function.arguments).input, code);
    assert.equal(calls[0].id, "code-1");
    assert.equal(calls[1].function.name, input.tools[1].function.name);
    assert.deepEqual(input.messages.filter(message => message.role === "tool").map(message => message.tool_call_id), ["code-1", "read-1"]);
    return completion({ content: "done" });
  });
  const stream = await events(await invoke({ instructions: "Follow instructions", max_output_tokens: 123,
    input: [{ type: "additional_tools", tools: [custom, namespace] },
      { role: "developer", content: [{ type: "input_text", text: "Policy" }] },
      { role: "user", content: "Read" },
      { type: "custom_tool_call", call_id: "code-1", name: "exec", input: code },
      { type: "function_call", call_id: "read-1", namespace: "files", name: "read", arguments: '{"path":"x"}' },
      { type: "custom_tool_call_output", call_id: "code-1", output: "ok" },
      { type: "function_call_output", call_id: "read-1", output: [{ type: "input_text", text: "file" }] }] }));
  assert.equal(stream[0].type, "response.created");
  assert.equal(stream.at(-1).type, "response.completed");
  assert.equal(stream.at(-1).response.output[0].content[0].text, "done");
  assert.equal(stream.at(-1).response.usage.total_tokens, 14);
  assert.equal(stream.at(-1).response.end_turn, true);
  assert.deepEqual(stream.map(event => event.sequence_number), stream.map((_, index) => index));
});

test("round trips custom, function and tool-search calls with original identities", async () => {
  const invoke = fixture(async (_model, input) => completion({ content: null, tool_calls: input.tools.map((tool, index) => ({
    id: `call-${index}`, type: "function", function: { name: tool.function.name,
      arguments: JSON.stringify(index === 0 ? { input: "text(42)" } : index === 1 ? { path: "a" } : { query: "calendar" }) },
  })) }, "tool_calls"));
  const stream = await events(await invoke({ tools: [custom, namespace, search], input: "hello" }));
  const output = stream.at(-1).response.output;
  assert.equal(output[0].type, "custom_tool_call");
  assert.equal(output[0].name, "exec");
  assert.equal(output[0].input, "text(42)");
  assert.equal(output[1].type, "function_call");
  assert.equal(output[1].namespace, "files");
  assert.equal(output[1].name, "read");
  assert.deepEqual(output[2].arguments, { query: "calendar" });
  assert.equal(output[2].execution, "client");
  assert.equal(output[2].type, "tool_search_call");
  assert.equal(stream.at(-1).response.end_turn, false);
  assert.equal(stream.find(event => event.type === "response.custom_tool_call_input.delta").delta, "text(42)");
});

test("deferred search results become callable tools on the next request", async () => {
  const invoke = fixture(async (_model, input) => {
    assert.equal(input.tools.length, 2);
    assert.equal(input.messages[2].role, "tool");
    return completion({ tool_calls: [{ id: "read", function: { name: input.tools[1].function.name, arguments: "{}" } }] }, "tool_calls");
  });
  const stream = await events(await invoke({ tools: [search], input: [
    { role: "user", content: "find tools" },
    { type: "tool_search_call", call_id: "search", execution: "client", arguments: { query: "files" } },
    { type: "tool_search_output", call_id: "search", execution: "client", tools: [namespace] },
  ] }));
  assert.equal(stream.at(-1).response.output[0].namespace, "files");
});

test("aliases cannot collide across tool namespaces or top-level names", async () => {
  const invoke = fixture(async (_model, input) => {
    assert.equal(new Set(input.tools.map(tool => tool.function.name)).size, 3);
    return completion({ content: "ok" });
  });
  await invoke({ tools: [namespace, { ...namespace, name: "other" }, { type: "function", name: "files_read" }], input: "hi" });
});

test("opaque compaction, delta histories and unsupported inputs fail before inference", async () => {
  const invoke = fixture(() => assert.fail("must not invoke AI"));
  await assert.rejects(invoke({ previous_response_id: "resp_old", input: [] }), /complete Responses history/);
  for (const type of ["compaction", "compaction_summary", "context_compaction", "compaction_trigger"]) {
    await assert.rejects(invoke({ input: [{ type, encrypted_content: "opaque" }] }), /compaction is unsupported/);
  }
  await assert.rejects(invoke({ input: [{ role: "user", content: [{ type: "input_image", image_url: "x" }] }] }), /unsupported content/);
  await assert.rejects(invoke({ input: [{ type: "function_call_output", call_id: "missing", output: "x" }] }), /no matching call/);
  await assert.rejects(invoke({ input: [{ type: "function_call", name: "f", call_id: "pending", arguments: "{}" }] }), /without outputs/);
});

test("truncated completions are terminal incomplete, never successful", async () => {
  const stream = await events(await fixture(async () => completion({ content: "partial" }, "length"))({ input: "hello" }));
  assert.equal(stream.at(-1).type, "response.incomplete");
  assert.equal(stream.at(-1).response.incomplete_details.reason, "max_output_tokens");
  assert.equal(stream.at(-1).response.end_turn, false);
});

test("provider errors and malformed tools fail explicitly", async () => {
  await assert.rejects(fixture(async () => { throw new Error("overloaded"); })({ input: "hi" }), /overloaded/);
  await assert.rejects(fixture(async () => ({}))({ input: "hi" }), /invalid chat completion/);
  await assert.rejects(fixture(async () => completion({ tool_calls: [{ function: { name: "unknown", arguments: "{}" } }] }, "tool_calls"))({ input: "hi" }), /unknown tool alias/);
  await assert.rejects(fixture(async () => completion({ tool_calls: [{ function: { name: "tool_0", arguments: "{}" } }] }, "tool_calls"))({ tools: [custom], input: "hi" }), /string input/);
});

test("cancellation rejects before dispatch and while inference is pending", async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(fixture(() => assert.fail("must not run"))({ input: "hi" }, controller.signal), { name: "AbortError" });
  const running = new AbortController();
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  const response = fixture(() => { started(); return new Promise(() => {}); })({ input: "hi" }, running.signal);
  await ready;
  running.abort();
  await assert.rejects(response, { name: "AbortError" });
});

test("agent messages preserve their routing metadata; opaque content fails", async () => {
  await fixture(async (_model, input) => {
    assert.deepEqual(JSON.parse(input.messages[0].content), { author: "child", recipient: "parent", message: "done" });
    return completion({ content: "received" });
  })({ input: [{ type: "agent_message", author: "child", recipient: "parent", content: [{ type: "input_text", text: "done" }] }] });
  await assert.rejects(fixture(() => assert.fail("must not run"))({ input: [{ type: "agent_message", content: [{ type: "encrypted_content", encrypted_content: "opaque" }] }] }), /unsupported content/);
});

test("endpoint and authorization are constrained to the local host-managed seam", async () => {
  const transport = createWorkersAiResponses({ run: () => assert.fail("must not run") });
  const request = { authorization: "host_managed", body: "{}", signal: new AbortController().signal };
  await assert.rejects(transport.createResponse(`${transport.apiBaseUrl}/responses/compact`, "session", request), /compaction is not supported/);
  await assert.rejects(transport.createResponse(`${transport.apiBaseUrl}/responses`, "session", { ...request, authorization: "bearer" }), /hostManaged/);
});

test("rejects mismatched models and unsupported effort before inference", async () => {
  const invoke = fixture(() => assert.fail("must not run"));
  await assert.rejects(invoke({ model: "gpt-6-astra", input: "hi" }), /unsupported model/);
  for (const effort of ["none", "xhigh", "max", null, 1]) {
    await assert.rejects(invoke({ reasoning: { effort }, input: "hi" }), /unsupported reasoning effort/);
    await assert.rejects(invoke({ input: [{ type: "configuration_update", reasoning: { effort } }] }), /unsupported reasoning effort/);
  }
});

test("partial configuration updates preserve the most recent supported effort", async () => {
  await fixture(async (_model, input) => {
    assert.equal(input.reasoning_effort, "medium");
    return completion({ content: "ok" });
  })({ model: "@cf/zai-org/glm-5.3", reasoning: { effort: "high" }, input: [
    { type: "configuration_update", reasoning: { effort: "medium" } },
    { type: "configuration_update", reasoning: {} },
    { type: "configuration_update" },
    { role: "user", content: "hi" },
  ] });
});

test("reasoning SSE parts have paired added and done events", async () => {
  const stream = await events(await fixture(async () => completion({ reasoning_content: "Consider the evidence.", content: "done" }))({ input: "hi" }));
  const reasoning = stream.find(event => event.type === "response.output_item.done" && event.item.type === "reasoning").item;
  const added = stream.find(event => event.type === "response.content_part.added" && event.item_id === reasoning.id);
  const done = stream.find(event => event.type === "response.content_part.done" && event.item_id === reasoning.id);
  assert.equal(added.part.type, "reasoning_text");
  assert.equal(added.part.text, "");
  assert.deepEqual(done.part, reasoning.content[0]);
  assert.ok(added.sequence_number < done.sequence_number);
});


test("accepts GLM's exact original tool name only when registered and unambiguous", async () => {
  const invoke = fixture(async () => completion({ tool_calls: [{ id: "live-read", function: { name: "read", arguments: '{"name":"circuit.json"}' } }] }, "tool_calls"));
  const out = await events(await invoke({ input: "read circuit", tools: [{ type: "function", name: "read", parameters: { type: "object" } }] }));
  const item = out.find(e => e.type === "response.output_item.done").item;
  assert.equal(item.type, "function_call");
  assert.equal(item.name, "read");
  assert.equal(item.call_id, "live-read");
  const duplicate = { type: "namespace", name: "other", tools: namespace.tools };
  await assert.rejects(invoke({ input: "read circuit", tools: [namespace, duplicate] }), /unknown tool alias/);
  const qualified = fixture(async () => completion({ tool_calls: [{ function: { name: "files.read", arguments: "{}" } }] }, "tool_calls"));
  const qualifiedOut = await events(await qualified({ input: "read", tools: [namespace, duplicate] }));
  assert.equal(qualifiedOut.find(e => e.type === "response.output_item.done").item.namespace, "files");
});

test("malformed or truncated provider tools never dispatch", async () => {
  for (const [message, reason] of [
    [{ content: { text: "silently lost" } }, "stop"],
    [{ tool_calls: {} }, "tool_calls"],
    [{ refusal: "no" }, "stop"],
    [{ tool_calls: [null] }, "tool_calls"],
    [{ tool_calls: [{ id: 42, function: { name: "tool_0", arguments: "{}" } }] }, "tool_calls"],
    [{ tool_calls: [{ function: { name: "tool_0", arguments: "null" } }] }, "tool_calls"],
    [{ tool_calls: [{ function: { name: "tool_0", arguments: "[]" } }] }, "tool_calls"],
    [{ tool_calls: [{ function: { name: "tool_0", arguments: "{}" } }] }, "length"],
  ]) await assert.rejects(fixture(async () => completion(message, reason))({ tools: [{ type: "function", name: "read" }], input: "hi" }));
  await assert.rejects(fixture(async () => ({ ...completion({ content: "ok" }), error: { message: "bad" } }))({ input: "hi" }), /invalid chat completion/);
});

test("rejects duplicate history IDs and interleaved tool batches before inference", async () => {
  const call = id => ({ type: "function_call", call_id: id, name: "f", arguments: "{}" });
  const output = id => ({ type: "function_call_output", call_id: id, output: "ok" });
  const invoke = fixture(() => assert.fail("must not infer"));
  await assert.rejects(invoke({ input: [call("a"), output("a"), call("a"), output("a")] }), /duplicate tool call ID/);
  await assert.rejects(invoke({ input: [call("a"), call("b"), output("a"), call("c"), output("b"), output("c")] }), /all outputs/);
  await assert.rejects(invoke({ input: "hi", text: { format: { type: "json_schema" } } }), /structured output/);
  await assert.rejects(invoke({ input: "hi", tool_choice: "invalid" }), /tool_choice/);
});
